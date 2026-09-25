#define _GNU_SOURCE
/* Bounded SSC research collector: discovery, reads, explicit sleep trials,
 * and explicit clock synchronization. Supervisor restores sleep trials.
 * Protobuf fields come from this watch's /system/etc/sensors/proto schemas.
 * QMI envelope: SNS_CLIENT_API v1.2 (request 0x20, payload TLV 1).
 * Android/bionic helper: build with NDK, use /system/bin/linker.
 */
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <stdlib.h>
#include <pthread.h>
#include <sys/timerfd.h>
#include "ssc_worker.h"
#include "ssc_limits.h"
#include "ssc_wake.h"
#include "ssc_owner.h"
#include "minute_tracker.h"
#include "time_payload.h"
#include "ssc_wait.h"
#include "ssc_notify.h"
#include "ssc_inventory.h"
#include "ssc_config.h"
#include "ssc_controls.h"
#include "ssc_extra_reads.h"
static struct ssc_config config_snapshot;
static int capture_config;
/* Optional long-lived sleep endpoint attachment; getters do not change config.
 * Reply receipt proves attachment/query only, not delivery of future sleep events. */
static struct ssc_config sleep_observers[2];
static int observe_sleep;
struct polled_read {
 const char *env,*tag,*text;
 unsigned request,reply;
 unsigned char suid[18];
 struct ssc_config snapshot;
};
static struct polled_read polled_reads[]={
 {.env="SSC_RHR_SUID",.tag="rhr",.request=1234,.reply=1029},
 {.env="SSC_WORKOUT_SUID",.tag="workout",.request=779,.reply=779}
};
#define POLLED_COUNT (sizeof polled_reads/sizeof *polled_reads)
static pthread_mutex_t config_mutex=PTHREAD_MUTEX_INITIALIZER;
static struct ssc_discovery discovery;
static int discovering;
static pthread_mutex_t discovery_mutex=PTHREAD_MUTEX_INITIALIZER;
static struct ssc_wait shutdown_wait;
static struct ssc_worker worker;
static struct ssc_wake wake_guard;
static struct minute_tracker minutes;
static pthread_mutex_t minute_mutex=PTHREAD_MUTEX_INITIALIZER;
static int track_minutes;
static int archive(unsigned kind,unsigned id,const void *data,unsigned len) {
 return sw_push(&worker,kind,id,data,len);
}
static int finish_archive(void) {
 if(shutdown_wait.stopped) {
  const char marker[]="shutdown_signal";
  archive(4,(unsigned)shutdown_wait.stopped,marker,sizeof marker-1);
 }
 int rc=sw_finish(&worker);
 if(shutdown_wait.error)rc=shutdown_wait.error;
 printf("ARCHIVE result=%d accepted=%llu durable=%llu rejected=%llu queue_high_water=%u queue_capacity=%u byte_high_water=%zu byte_capacity=%u\n",rc,
  (unsigned long long)worker.accepted,(unsigned long long)worker.durable,
  (unsigned long long)worker.rejected,worker.high_water,SW_CAPACITY,worker.byte_high_water,SW_BYTE_CAPACITY);
 swake_close(&wake_guard);
 return rc!=0;
}
static int kernel_uuid(const char *path,char out[37]) {
 int fd=open(path,O_RDONLY|O_CLOEXEC);
 if(fd<0) return -1;
 ssize_t n=read(fd,out,36);close(fd);
 if(n!=36) return -1;
 for(unsigned i=0;i<36;i++) {
  if(i==8||i==13||i==18||i==23) { if(out[i]!='-')return -1; }
  else if(!((out[i]>='0'&&out[i]<='9')||(out[i]>='a'&&out[i]<='f')))return -1;
 }
 out[36]=0;return 0;
}
static int stopping(void) { return swait_ms(&shutdown_wait,0)!=0; }
static int wait_boottime(unsigned seconds) {
 return swait_ms(&shutdown_wait,seconds*1000)<0?-1:0;
}
typedef void (*ind_fn)(void *,unsigned,void *,unsigned,void *);
static void dump(const char *kind,unsigned id,const void *data,unsigned len) {
 archive(!strcmp(kind,"IND")?3:2,id,data,len);

}
static void indication(void *c,unsigned id,void *data,unsigned len,void *ctx) {
 (void)c;(void)ctx;if(len<=65536)dump("IND",id,data,len);else archive(3,id,data,len);
 if(capture_config) {
  pthread_mutex_lock(&config_mutex);
  sc_feed(&config_snapshot,id,data,len);
  pthread_mutex_unlock(&config_mutex);
 }
 for(unsigned i=0;i<POLLED_COUNT;i++)if(polled_reads[i].text) {
  pthread_mutex_lock(&config_mutex);
  sc_feed(&polled_reads[i].snapshot,id,data,len);
  pthread_mutex_unlock(&config_mutex);
 }
 if(observe_sleep) {
  pthread_mutex_lock(&config_mutex);
  for(unsigned i=0;i<2;i++)if(!sleep_observers[i].seen)
   sc_feed(&sleep_observers[i],id,data,len);
  pthread_mutex_unlock(&config_mutex);
 }
 if(discovering) {
  pthread_mutex_lock(&discovery_mutex);
  sd_feed(&discovery,id,data,len);
  pthread_mutex_unlock(&discovery_mutex);
 }
 if(track_minutes) {
  pthread_mutex_lock(&minute_mutex);
  mt_feed(&minutes,id,data,len);
  pthread_mutex_unlock(&minute_mutex);
 }
}
static size_t field(unsigned char *p,unsigned tag,const void *data,size_t n) {
 /* All fixed discovery requests have field lengths <128. */
 if(n>=128) abort();p[0]=tag;p[1]=n;memcpy(p+2,data,n);return n+2;
}
static size_t query(unsigned char *out,const char *name) {
 unsigned char suid[18]={9}, lookup[100],req[110],pb[200];
 memset(suid+1,0xab,8);suid[9]=17;memset(suid+10,0xab,8);
 size_t k=field(lookup,10,name,strlen(name));
 lookup[k++]=16;lookup[k++]=0; /* no updates */
 lookup[k++]=24;lookup[k++]=0; /* all matching SUIDs */
 size_t r=field(req,18,lookup,k), n=field(pb,10,suid,sizeof suid);
 const unsigned char msg[]={21,0,2,0,0}; /* fixed32 message 512 */
 memcpy(pb+n,msg,sizeof msg);n+=sizeof msg;
 const unsigned char susp[]={8,1,16,0}; /* APSS, WAKEUP */
 n+=field(pb+n,26,susp,sizeof susp);n+=field(pb+n,34,req,r);
 /* variable byte array is prefixed by uint16 count within QMI TLV. */
 out[0]=1;out[1]=(n+2)&255;out[2]=(n+2)>>8;
 out[3]=n&255;out[4]=n>>8;memcpy(out+5,pb,n);return n+5;
}
static int parse_suid(const char *text,unsigned char out[18]) {
 if(!text || strlen(text)!=36)return -1;
 for(unsigned i=0;i<18;i++) {
  unsigned value=0;
  for(unsigned j=0;j<2;j++) {
   unsigned c=(unsigned char)text[i*2+j];
   unsigned digit=c>='0'&&c<='9'?c-'0':c>='a'&&c<='f'?c-'a'+10:c>='A'&&c<='F'?c-'A'+10:99;
   if(digit>15)return -1;
   value=value*16+digit;
  }
  out[i]=(unsigned char)value;
 }
 return out[0]==9 && out[9]==17?0:-1;
}
static size_t minute_request(unsigned char *out,const unsigned char suid[18],int clocked,int offset) {
 unsigned char pb[100];
 size_t n=field(pb,10,suid,18);
 const unsigned char msg[]={21,0,3,0,0},susp[]={8,1,16,0},get[]={18,2,8,1};
 memcpy(pb+n,msg,sizeof msg);n+=sizeof msg;
 n+=field(pb+n,26,susp,sizeof susp);
 if(clocked) {
  struct timespec now;unsigned char data[40],wrapped[48];size_t size=0;
  if(clock_gettime(CLOCK_REALTIME,&now) || now.tv_sec<0 ||
     ssc_minute_time_payload(data,sizeof data,(uint64_t)now.tv_sec,offset,&size))return 0;
  size_t wrapped_size=field(wrapped,18,data,size);
  n+=field(pb+n,34,wrapped,wrapped_size);
 } else n+=field(pb+n,34,get,sizeof get);
 out[0]=1;sj_le(out+1,n+2,2);sj_le(out+3,n,2);memcpy(out+5,pb,n);return n+5;
}
int main(int argc, char **argv) {
 int cleanup=argc==2 && !strcmp(argv[1],"--cleanup");
 int owner=sowner_acquire("/run/hoki-ssc-recording.lock");
 if(owner<0) {
  if(cleanup && (errno==EWOULDBLOCK || errno==EAGAIN)) {
   puts("CLEANUP_SKIPPED_ACTIVE_OWNER");return 0;
  }
  perror("SSC owner");return 1;
 }
 if(cleanup) {
  int rc=swake_open(&wake_guard);
  if(!rc) { wake_guard.maybe_held=1;rc=swake_release(&wake_guard); }
  swake_close(&wake_guard);close(owner);return rc?1:0;
 }
 if(swait_init(&shutdown_wait)) { perror("signal waiter");return 1; }
 unsigned request_id=1,config_reply=0,sleep_seconds=60; int minute_read=0,sleep_trial=0,minute_poll=0,continuous=0,time_sync=0,time_offset=0;
 unsigned char attr_suid[18],minute_suid[18],sleep_suid[18],control_payload[8];
 size_t control_size=0;
 if(argc!=1 && argc!=3)return 64;
 if(argc==3){
  if(!strcmp(argv[1],"--minute-read")){request_id=768;minute_read=1;}
  else if(!strcmp(argv[1],"--minute-poll")){request_id=768;minute_read=1;minute_poll=1;}
  else if(!strcmp(argv[1],"--minute-record")){request_id=768;minute_read=1;minute_poll=1;continuous=1;}
  else if(!strcmp(argv[1],"--sleep-trial")){
   if(!getenv("SSC_SLEEP_RESTORE_ARMED"))return 64;
   request_id=776;sleep_trial=1;
   const char *seconds=getenv("SSC_SLEEP_SECONDS");
   if(seconds) {
    char *end;unsigned long value=strtoul(seconds,&end,10);
    if(!*seconds || *end || value<1 || value>180)return 64;
    sleep_seconds=(unsigned)value;
   }
  }
  else if(!strcmp(argv[1],"--time-sync") || !strcmp(argv[1],"--minute-read-clock") || !strcmp(argv[1],"--minute-record-clock")) {
   request_id=768;time_sync=!strcmp(argv[1],"--time-sync")?1:2;
   const char *offset=getenv("SSC_TIME_OFFSET_SECONDS");char *end;
   if(!offset || !*offset)return 64;
   errno=0;long value=strtol(offset,&end,10);
   if(errno || *end || value < -43200 || value > 50400 || value%3600)return 64;
   time_offset=(int)value;
   if(!strcmp(argv[1],"--minute-record-clock")) { minute_read=1;minute_poll=1;continuous=1; }
  }
  else if(!strcmp(argv[1],"--set-tracking") || !strcmp(argv[1],"--set-detect") || !strcmp(argv[1],"--set-permissions")) {
   const char *armed=getenv("SSC_CONFIG_RESTORE_ARMED");
   if(!armed || strcmp(armed,"1") || sctl_payload(argv[1],getenv("SSC_CONFIG_VALUE"),getenv("SSC_RHR_PERMISSION"),getenv("SSC_SLEEP_PERMISSION"),&request_id,control_payload,&control_size))return 64;
  }
  else if(!strcmp(argv[1],"--rhr-read")){request_id=1234;capture_config=1;}
  else if(!strcmp(argv[1],"--workout-summary")){request_id=779;capture_config=1;}
  else if(!strcmp(argv[1],"--user-config")){request_id=769;capture_config=1;}
  else if(!strcmp(argv[1],"--tracking-config")){request_id=776;capture_config=1;}
  else if(!strcmp(argv[1],"--detect-config")){request_id=876;capture_config=1;}
  else if(!ser_plan(argv[1],&request_id,&config_reply,control_payload,&control_size)){capture_config=1;}
  else if(strcmp(argv[1],"--attributes"))return 64;
  if(parse_suid(argv[2],attr_suid))return 64;
  if(capture_config && sc_init(&config_snapshot,attr_suid,config_reply?config_reply:(request_id==1234?1029:request_id)))return 64;
 }
 for(unsigned i=0;i<POLLED_COUNT;i++) {
  struct polled_read *p=&polled_reads[i];p->text=getenv(p->env);
  if(p->text && (!continuous || parse_suid(p->text,p->suid) || sc_init(&p->snapshot,p->suid,p->reply)))return 64;
 }
 const char *sleep_text=getenv("SSC_SLEEP_OBSERVE_SUID");
 if(sleep_text) {
  if(!continuous || parse_suid(sleep_text,sleep_suid) ||
     sc_init(&sleep_observers[0],sleep_suid,776) ||
     sc_init(&sleep_observers[1],sleep_suid,876))return 64;
  observe_sleep=1;
 }
 if(getenv("NOTIFY_SOCKET") && !continuous) {
  fprintf(stderr,"readiness requires --minute-record\n");return 64;
 }
 const char *minute_text=minute_poll?argv[2]:(sleep_trial?getenv("SSC_MINUTE_SUID"):NULL);
 if(minute_text) {
  if(parse_suid(minute_text,minute_suid) || mt_init(&minutes,minute_suid,18))return 64;
  track_minutes=1;
 }
 discovering=argc==1;
 const char *directory=getenv("SSC_JOURNAL_DIR");
 if(!directory) { fprintf(stderr,"SSC_JOURNAL_DIR required\n"); return 64; }
 uint64_t journal_limit;
 if(sl_budget(getenv("SSC_JOURNAL_LIMIT_BYTES"),continuous,&journal_limit))return 64;
 int journal_rc=swake_open(&wake_guard);
 if(!journal_rc) journal_rc=sw_start(&worker,directory,journal_limit,256*1024*1024,
                                  swake_hold,swake_release,&wake_guard);
 if(journal_rc) {
  fprintf(stderr,"journal startup error %d\n",journal_rc);
  swake_close(&wake_guard);return 1;
 }
 char boot_id[37],session_id[37],metadata[256];
 if(kernel_uuid("/proc/sys/kernel/random/boot_id",boot_id) ||
    kernel_uuid("/proc/sys/kernel/random/uuid",session_id)) {
  fprintf(stderr,"session identity unavailable\n");return 1;
 }
 int meta_len=snprintf(metadata,sizeof metadata,
  "session_v1\nsession_id=%s\nboot_id=%s\nmode=%s\nsuid=%s\n",
  session_id,boot_id,argc==3?argv[1]:"discovery",argc==3?argv[2]:"lookup");
 if(meta_len<0 || (size_t)meta_len>=sizeof metadata ||
    sw_push(&worker,4,0,metadata,(size_t)meta_len))return 1;
 meta_len=snprintf(metadata,sizeof metadata,"storage_policy_v1\nlimit_bytes=%llu\nreserve_bytes=268435456\nrotation=none\n",(unsigned long long)journal_limit);
 if(meta_len<0 || (size_t)meta_len>=sizeof metadata || sw_push(&worker,4,0,metadata,(size_t)meta_len))return 1;
 meta_len=snprintf(metadata,sizeof metadata,"queue_policy_v2\nmessage_capacity=%u\npayload_capacity_bytes=%u\nmax_message_bytes=65536\ninflight_payload_bytes=65536\nworker_bytes=%zu\n",SW_CAPACITY,SW_BYTE_CAPACITY,sizeof worker);
 if(meta_len<0 || (size_t)meta_len>=sizeof metadata || sw_push(&worker,4,0,metadata,(size_t)meta_len))return 1;
 if(track_minutes) {
  meta_len=snprintf(metadata,sizeof metadata,"minute_poll_v1\nsuid=%s\ninterval_seconds=30\ncontinuous=%d\n",minute_text,continuous);
  if(meta_len<0 || (size_t)meta_len>=sizeof metadata ||
     sw_push(&worker,4,0,metadata,(size_t)meta_len))return 1;
 }
 if(observe_sleep) {
  meta_len=snprintf(metadata,sizeof metadata,"sleep_observer_v1\nsuid=%s\nrequests=776,876\nevent_delivery_verified=0\n",sleep_text);
  if(meta_len<0 || (size_t)meta_len>=sizeof metadata ||
     sw_push(&worker,4,0,metadata,(size_t)meta_len))return 1;
 }
 for(unsigned i=0;i<POLLED_COUNT;i++)if(polled_reads[i].text) {
  struct polled_read *p=&polled_reads[i];
  meta_len=snprintf(metadata,sizeof metadata,"%s_poll_v1\nsuid=%s\nrequest=%u\nreply=%u\nshared_minute_schedule=1\nfreshness_verified=0\n",p->tag,p->text,p->request,p->reply);
  if(meta_len<0 || (size_t)meta_len>=sizeof metadata || sw_push(&worker,4,0,metadata,(size_t)meta_len))return 1;
 }
 setvbuf(stdout,NULL,_IOLBF,0);
 void *lib=dlopen("/vendor/lib/libssc.so",RTLD_NOW);
 void *qmi=dlopen("/vendor/lib/libqmi_cci.so",RTLD_NOW);
 if(!lib||!qmi){fprintf(stderr,"dlopen: %s\n",dlerror());return 1;}
 void *(*getsvc)(int,int,int)=dlsym(lib,"SNS_CLIENT_SVC_get_service_object_internal_v01");
 int (*init)(void*,unsigned,ind_fn,void*,void*,unsigned,void**)=dlsym(qmi,"qmi_client_init_instance");
 int (*release)(void*)=dlsym(qmi,"qmi_client_release");
 int (*sendraw)(void*,unsigned,void*,unsigned,void*,unsigned,unsigned*,unsigned)=dlsym(qmi,"qmi_client_send_raw_msg_sync");
 if(!getsvc||!init||!release||!sendraw)return 2;
 void *svc=getsvc(1,2,6), *client=NULL;
 if(!svc){fprintf(stderr,"Unexpected QMI service version\n");return 3;}
 int rc=init(svc,0xffff,indication,NULL,NULL,5000,&client);
 printf("INIT %d\n",rc);if(rc)return 4;

 int failed=0;
 if(stopping()) {
  if(release(client))return 1;
  return finish_archive();
 }
 if(argc==3){
  unsigned char req[256],pb[200],resp[2048];unsigned rn=0;
  size_t n=field(pb,10,attr_suid,18);
  const unsigned char msg[]={21,request_id&255,(request_id>>8)&255,0,0}, susp[]={8,1,16,0};
  memcpy(pb+n,msg,sizeof msg);n+=sizeof msg;
  n+=field(pb+n,26,susp,sizeof susp);
  const unsigned char empty_attr[]={18,0}, read_min[]={18,2,8,1};
  if(control_size) {
   unsigned char wrapped[16];size_t wrapped_size=field(wrapped,18,control_payload,control_size);
   n+=field(pb+n,34,wrapped,wrapped_size);
  } else if(time_sync) {
   struct timespec now;unsigned char time_data[32],wrapped[40];size_t time_size;
   if(clock_gettime(CLOCK_REALTIME,&now) || now.tv_sec<0 ||
      (time_sync==2?ssc_minute_time_payload:ssc_time_payload)(time_data,sizeof time_data,(uint64_t)now.tv_sec,time_offset,&time_size)) {
    if(release(client))return 1;
    finish_archive();return 1;
   }
   size_t wrapped_size=field(wrapped,18,time_data,time_size);
   n+=field(pb+n,34,wrapped,wrapped_size);
  } else {
   n+=field(pb+n,34,(minute_read||sleep_trial)?read_min:empty_attr,
            (minute_read||sleep_trial)?sizeof read_min:sizeof empty_attr);
  }
  req[0]=1;req[1]=n+2;req[2]=0;req[3]=n;req[4]=0;memcpy(req+5,pb,n);
  int request_archive_error=archive(1,0x20,req,n+5);
  if(!request_archive_error && (control_size || time_sync || sleep_trial)) {
   request_archive_error=sw_barrier(&worker,5000);
   if(!request_archive_error && stopping())request_archive_error=ECANCELED;
  }
  if(minute_poll) {
   pthread_mutex_lock(&minute_mutex);mt_begin(&minutes);pthread_mutex_unlock(&minute_mutex);
  }
  rc=request_archive_error?request_archive_error:sendraw(client,0x20,req,n+5,resp,sizeof resp,&rn,3000);
  printf("ATTR_SEND %d\n",rc);
  if(!rc&&rn<=sizeof resp)dump("RSP",0x20,resp,rn);else failed=1;
  if(sleep_trial && !failed && !stopping()) {
   /* Fixed envelope: TLV/count5 + SUID field20 + msg key1 -> fixed32 at26. */
   sj_le(req+26,876,4);
   int request_archive_error=archive(1,0x20,req,n+5);
   if(!request_archive_error)request_archive_error=sw_barrier(&worker,5000);
   if(!request_archive_error && stopping())request_archive_error=ECANCELED;
   rc=request_archive_error?request_archive_error:sendraw(client,0x20,req,n+5,resp,sizeof resp,&rn,3000);
   printf("DETECT_SEND %d\n",rc);
   if(!rc&&rn<=sizeof resp)dump("RSP",0x20,resp,rn);else failed=1;
  }
  if(observe_sleep && !failed) {
   for(unsigned which=0;which<2 && !failed && !stopping();which++) {
    unsigned char watch_pb[64],watch_req[80];
    size_t watch_n=field(watch_pb,10,sleep_suid,18);
    unsigned message=which?876:776;
    unsigned char watch_msg[]={21,message&255,(message>>8)&255,0,0};
    const unsigned char watch_susp[]={8,1,16,0},watch_empty[]={18,0};
    memcpy(watch_pb+watch_n,watch_msg,sizeof watch_msg);watch_n+=sizeof watch_msg;
    watch_n+=field(watch_pb+watch_n,26,watch_susp,sizeof watch_susp);
    watch_n+=field(watch_pb+watch_n,34,watch_empty,sizeof watch_empty);
    watch_req[0]=1;sj_le(watch_req+1,watch_n+2,2);sj_le(watch_req+3,watch_n,2);
    memcpy(watch_req+5,watch_pb,watch_n);
    int e=archive(1,0x20,watch_req,watch_n+5);
    rc=e?e:sendraw(client,0x20,watch_req,watch_n+5,resp,sizeof resp,&rn,3000);
    printf("SLEEP_OBSERVER_SEND %u %d\n",message,rc);
    if(!rc&&rn<=sizeof resp)dump("RSP",0x20,resp,rn);else failed=1;
   }
  }
  if(track_minutes && !failed) {
   /* Research schedule with explicit continuous mode. Timer does not wake the CPU from suspend;
    * the HAL/RTC controller supplies wakeups. Never catch up with bursts. */
   struct timespec started,now;
   int initial_minute=minute_poll,ready_reported=0;
   if(clock_gettime(CLOCK_BOOTTIME,&started))failed=1;
   while(!failed && !stopping()) {
    if(clock_gettime(CLOCK_BOOTTIME,&now)) { failed=1;break; }
    if(!continuous && now.tv_sec-started.tv_sec>=(time_t)sleep_seconds)break;
    if(initial_minute)initial_minute=0;
    else {
     pthread_mutex_lock(&minute_mutex);
     int begin_error=mt_begin(&minutes);
     pthread_mutex_unlock(&minute_mutex);
     if(begin_error) { failed=1;break; }
     size_t size=minute_request(req,minute_suid,time_sync==2,time_offset);
     if(!size) { failed=1;break; }
     int request_archive_error=archive(1,0x20,req,(unsigned)size);
     if(!request_archive_error && time_sync==2)request_archive_error=sw_barrier(&worker,5000);
     if(!request_archive_error && stopping())request_archive_error=ECANCELED;
     rc=request_archive_error?request_archive_error:sendraw(client,0x20,req,size,resp,sizeof resp,&rn,3000);
     printf("MINUTE_SEND %d\n",rc);
     if(!rc&&rn<=sizeof resp)dump("RSP",0x20,resp,rn);else failed=1;
    }
    for(unsigned i=0;i<POLLED_COUNT && !failed && !stopping();i++)if(polled_reads[i].text) {
     struct polled_read *p=&polled_reads[i];
     pthread_mutex_lock(&config_mutex);
     int reset=sc_init(&p->snapshot,p->suid,p->reply);
     pthread_mutex_unlock(&config_mutex);
     if(reset) { failed=1;break; }
     unsigned char rp[64],rq[80];size_t z=field(rp,10,p->suid,18);
     const unsigned char mid[]={21,p->request&255,(p->request>>8)&255,0,0},rs[]={8,1,16,0},empty[]={18,0};
     memcpy(rp+z,mid,sizeof mid);z+=sizeof mid;
     z+=field(rp+z,26,rs,sizeof rs);z+=field(rp+z,34,empty,sizeof empty);
     rq[0]=1;sj_le(rq+1,z+2,2);sj_le(rq+3,z,2);memcpy(rq+5,rp,z);
     int e=archive(1,0x20,rq,z+5);
     rc=e?e:sendraw(client,0x20,rq,z+5,resp,sizeof resp,&rn,3000);
     printf("%s_READ_SEND %d\n",p->tag,rc);
     if(!rc&&rn<=sizeof resp)dump("RSP",0x20,resp,rn);else failed=1;
    }
    if(wait_boottime(30))failed=1;
    for(unsigned i=0;i<POLLED_COUNT;i++)if(polled_reads[i].text) {
     struct polled_read *p=&polled_reads[i];
     pthread_mutex_lock(&config_mutex);
     int complete=p->snapshot.seen==1 && !p->snapshot.error;
     pthread_mutex_unlock(&config_mutex);
     if(!complete) { fprintf(stderr,"%s reply missing/invalid\n",p->tag);failed=1; }
    }
    pthread_mutex_lock(&minute_mutex);
    int pending=minutes.pending,error=minutes.error;
    unsigned completed=minutes.completed;
    pthread_mutex_unlock(&minute_mutex);
    if(pending||error)failed=1;
    if(observe_sleep) {
     pthread_mutex_lock(&config_mutex);
     int attached=sleep_observers[0].seen==1 && sleep_observers[1].seen==1 &&
                  !sleep_observers[0].error && !sleep_observers[1].error;
     pthread_mutex_unlock(&config_mutex);
     if(!attached) { fprintf(stderr,"sleep observer configuration replies missing/invalid\n");failed=1; }
    }
    if(continuous && completed && !ready_reported && !failed && !stopping()) {
     pthread_mutex_lock(&worker.mutex);
     int archive_error=worker.error;
     int durable=worker.accepted==worker.durable && !worker.rejected;
     pthread_mutex_unlock(&worker.mutex);
     if(archive_error)failed=1;
     else if(durable) {
      if(snotify_ready(getenv("NOTIFY_SOCKET"))) { perror("SSC readiness");failed=1; }
      else { ready_reported=1;puts("READY first minute transfer archived; freshness unverified"); }
     }
    }
   }
  } else if(wait_boottime(sleep_trial&&!failed?sleep_seconds:3))failed=1;
  /* Independent supervisor restores configuration, including abnormal exit. */
  rc=release(client);printf("RELEASE %d\n",rc);
  if(rc)return 1; /* callback quiescence uncertain: supervisor cleans up */
  if(track_minutes) {
   printf("MINUTE_TRANSFERS complete=%u pending=%d error=%d\n",minutes.completed,minutes.pending,minutes.error);
   if(minutes.pending||minutes.error)failed=1;
  }
  if(capture_config && (config_snapshot.error || config_snapshot.seen!=1 || shutdown_wait.stopped))failed=1;
  archive(4,0,failed?"request_failed":"request_sent",failed?14:12);
  int journal_failed=finish_archive(),snapshot_failed=0;
  if(capture_config && !failed && !journal_failed) {
   snapshot_failed=swake_open(&wake_guard);
   if(!snapshot_failed)snapshot_failed=swake_hold(&wake_guard);
   if(!snapshot_failed)snapshot_failed=sc_publish(&config_snapshot,directory,boot_id,session_id,argv[1],attr_suid);
   swake_close(&wake_guard);
   if(wake_guard.error)snapshot_failed=1;
   if(snapshot_failed)perror("configuration snapshot");
  }
  return failed||journal_failed||snapshot_failed;
 }
 for(unsigned i=0;i<SD_COUNT && !stopping();i++){
  unsigned char req[256],resp[2048];unsigned rn=0;
  size_t n=query(req,sd_names[i]);printf("QUERY %s\n",sd_names[i]);
  int request_archive_error=archive(1,0x20,req,n);
  rc=request_archive_error?request_archive_error:sendraw(client,0x20,req,n,resp,sizeof resp,&rn,3000);
  printf("SEND %d\n",rc);if(!rc&&rn<=sizeof resp)dump("RSP",0x20,resp,rn);else failed=1;
  if(swait_ms(&shutdown_wait,500)<0){failed=1;break;}
 }
 if(wait_boottime(3))failed=1;
 rc=release(client);printf("RELEASE %d\n",rc);
 if(rc)return 1;
 archive(4,0,failed?"request_failed":"request_sent",failed?14:12);
 int journal_failed=finish_archive();
 int inventory_failed=0;
 if(!journal_failed && !failed) {
  inventory_failed=swake_open(&wake_guard);
  if(!inventory_failed)inventory_failed=swake_hold(&wake_guard);
  if(!inventory_failed)inventory_failed=sd_publish(&discovery,directory,boot_id,session_id,shutdown_wait.stopped);
  swake_close(&wake_guard);
  if(wake_guard.error)inventory_failed=1;
 }
 if(inventory_failed)perror("discovery inventory");
 return failed||journal_failed||inventory_failed||discovery.error;
}
