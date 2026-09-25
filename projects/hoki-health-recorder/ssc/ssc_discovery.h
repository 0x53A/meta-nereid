#ifndef SSC_DISCOVERY_H
#define SSC_DISCOVERY_H
#include "ssc_wire.h"
#include <string.h>
static const char *sd_names[]={"accel","fsl_sleep","fsl_min","fsl_cfg","fsl_usr","fsl_chrm","fsl_actrec","fsl_tracker","heart_rate","offbody_detect","ppg","sleep_wake","sac","ott","sensor_temperature","wrist_temperature","fsl_rhr","fsl_wk"
#ifdef SSC_EXTENDED_DISCOVERY
/* Research candidates from vendor proto basenames and stock library strings.
 * A name here is not a claim that this firmware advertises it. */
,"gyro","mag","pressure","ambient_light","ambient_temperature","humidity",
"proximity","heart_beat","pedometer","pedometer_wrist","rotv","game_rv",
"geomag_rv","gravity","motion_detect","sig_motion","wrist_tilt_gesture",
"tilt_to_wake","thermopile","hall","rgb","sar","spo2","calories","rr",
"rhr","fsl_hwf","fsl_hb_det","fsl_dvm_tracker"
#endif
};
#define SD_COUNT (sizeof sd_names/sizeof *sd_names)
struct sd_entry { unsigned responses,count; unsigned char suids[8][18]; };
struct ssc_discovery { struct sd_entry entries[SD_COUNT]; int error; };
static int sd_payload(struct ssc_discovery *d,const unsigned char *p,size_t n) {
 struct mt_field name,f,a,b;
 if(mt_get(p,n,1,2,&name)!=1)return -1;
 unsigned index;
 for(index=0;index<SD_COUNT;index++)if(strlen(sd_names[index])==name.len && !memcmp(sd_names[index],name.data,name.len))break;
 if(index==SD_COUNT)return -1;
 struct sd_entry next={.responses=1};
 if(d->entries[index].responses)return -1; /* no update registration: ambiguous repeated reply */
 const unsigned char *end=p+n;int rc;
 while((rc=mt_next(&p,end,&f))>0)if(f.number==2) {
  if(f.wire!=2 || next.count==8 || mt_get(f.data,f.len,1,1,&a)!=1 || mt_get(f.data,f.len,2,1,&b)!=1)return -1;
  unsigned char *out=next.suids[next.count];out[0]=9;out[9]=17;
  for(unsigned k=0;k<8;k++){out[1+k]=(unsigned char)(a.value>>(8*k));out[10+k]=(unsigned char)(b.value>>(8*k));}
  for(unsigned k=0;k<next.count;k++)if(!memcmp(out,next.suids[k],18))return -1;
  ++next.count;
 }
 if(rc<0)return -1;
 d->entries[index]=next;return 0;
}
static int sd_envelope(struct ssc_discovery *d,const unsigned char *p,size_t n) {
 struct mt_field source,a,b,event,id,payload,ticks;
 if(mt_get(p,n,1,2,&source)!=1 || mt_get(source.data,source.len,1,1,&a)!=1 || mt_get(source.data,source.len,2,1,&b)!=1)return -1;
 if(a.value!=UINT64_C(0xabababababababab) || b.value!=a.value)return 0;
 const unsigned char *end=p+n;int rc;
 while((rc=mt_next(&p,end,&event))>0)if(event.number==2) {
  if(event.wire!=2 || mt_get(event.data,event.len,1,5,&id)!=1 || mt_get(event.data,event.len,2,1,&ticks)!=1 || mt_get(event.data,event.len,3,2,&payload)!=1)return -1;
  if(id.value==768 && sd_payload(d,payload.data,payload.len))return -1;
 }
 return rc<0?-1:0;
}
static int sd_feed(struct ssc_discovery *d,unsigned message,const void *raw,size_t n) {
 if(d->error)return -1;
 if(message!=33 && message!=34)return 0;
 const unsigned char *p=raw;size_t pos=0;unsigned found=0;
 if(n>65536)goto bad;
 while(pos<n) {
  if(n-pos<3)goto bad;
  unsigned tag=p[pos],size=p[pos+1]|((unsigned)p[pos+2]<<8);pos+=3;
  if(size>n-pos)goto bad;
  if(tag==2 && (found++ || size<2 || (p[pos]|((unsigned)p[pos+1]<<8))!=size-2 || sd_envelope(d,p+pos+2,size-2)))goto bad;
  pos+=size;
 }
 if(found==1)return 0;
bad:d->error=1;return -1;
}
#endif
