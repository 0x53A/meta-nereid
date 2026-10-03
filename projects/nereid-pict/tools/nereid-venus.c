/* Hoki Venus SSH worker. Copyright 2026 Lukas Rieger <code@lukasrieger.com>
 * Based on the validated downstream V4L2/ION encoder experiment. */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
#include <time.h>
#include <poll.h>
#include <signal.h>
#include <sys/wait.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <linux/videodev2.h>
struct ion_alloc { uint64_t len; uint32_t mask,flags,fd,unused; };
#define ION_ALLOC _IOWR('I',0,struct ion_alloc)
#define MAXB 32
struct mem { int fd; unsigned len; unsigned char *ptr; int initialized; };
struct queue { unsigned type,n,np; struct mem m[MAXB][VIDEO_MAX_PLANES]; };
static int dev=-1,ion=-1; static unsigned w=416,h=416,fps=35;
static volatile sig_atomic_t stopping;
static struct queue in={.type=V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE},cap={.type=V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE};
static int streaming;
static void fail(const char *s){perror(s);exit(1);}
static void call(unsigned long op,void *arg,const char *s){if(ioctl(dev,op,arg)<0)fail(s);}
static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec+t.tv_nsec/1e9;}
static void format(unsigned type,unsigned fourcc){
 struct v4l2_format f={.type=type}; f.fmt.pix_mp.width=w;f.fmt.pix_mp.height=h;f.fmt.pix_mp.pixelformat=fourcc;f.fmt.pix_mp.field=V4L2_FIELD_NONE;
 call(VIDIOC_S_FMT,&f,"S_FMT");
 fprintf(stderr,"format type=%u %ux%u planes=%u size=%u stride=%u\n",type,f.fmt.pix_mp.width,f.fmt.pix_mp.height,f.fmt.pix_mp.num_planes,f.fmt.pix_mp.plane_fmt[0].sizeimage,f.fmt.pix_mp.plane_fmt[0].bytesperline);
}
static void allocq(struct queue *q){
 struct v4l2_requestbuffers r={.type=q->type,.memory=V4L2_MEMORY_USERPTR,.count=4};call(VIDIOC_REQBUFS,&r,"REQBUFS"); q->n=r.count;if(q->n>MAXB)exit(2);
 struct v4l2_format f={.type=q->type};call(VIDIOC_G_FMT,&f,"G_FMT");q->np=f.fmt.pix_mp.num_planes;if(q->np>VIDEO_MAX_PLANES)exit(2);
 fprintf(stderr,"buffers type=%u count=%u planes=%u\n",q->type,q->n,q->np);
 for(unsigned i=0;i<q->n;i++)for(unsigned p=0;p<q->np;p++){
  unsigned sz=f.fmt.pix_mp.plane_fmt[p].sizeimage;
  if(!sz)continue;
  struct ion_alloc a={.len=(sz+4095u)&~4095u,.mask=1u<<25};
  if(ioctl(ion,ION_ALLOC,&a)<0)fail("ION_ALLOC");
  struct mem *m=&q->m[i][p];m->fd=a.fd;m->len=a.len;m->ptr=mmap(NULL,m->len,PROT_READ|PROT_WRITE,MAP_SHARED,m->fd,0);if(m->ptr==MAP_FAILED)fail("mmap");memset(m->ptr,0,m->len);
 }
}
static void put(struct queue *q,unsigned i,unsigned seq){
 struct v4l2_plane p[VIDEO_MAX_PLANES]={0};struct v4l2_buffer b={.type=q->type,.memory=V4L2_MEMORY_USERPTR,.index=i,.length=q->np,.m.planes=p};
 for(unsigned j=0;j<q->np;j++){struct mem *m=&q->m[i][j];p[j].length=m->len;p[j].m.userptr=(unsigned long)m->ptr;p[j].reserved[0]=m->fd;}
 if(q->type==V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE){
  unsigned stride=(w+127)&~127u,lines=(h+31)&~31u;struct mem *m=&q->m[i][0];
  if((uint64_t)stride*(lines+((h/2+15)&~15u))>m->len)exit(3);
  p[0].bytesused=m->len;b.timestamp.tv_sec=seq/fps;b.timestamp.tv_usec=(seq%fps)*1000000/fps;
 }
 call(VIDIOC_QBUF,&b,"QBUF");
}
static void prepare(struct queue *q){
 for(unsigned i=0;i<q->n;i++){
  struct v4l2_plane p[VIDEO_MAX_PLANES]={0};
  struct v4l2_buffer b={.type=q->type,.memory=V4L2_MEMORY_USERPTR,.index=i,.length=q->np,.m.planes=p};
  for(unsigned j=0;j<q->np;j++){struct mem *m=&q->m[i][j];p[j].length=m->len;p[j].m.userptr=(unsigned long)m->ptr;p[j].reserved[0]=m->fd;}
  call(VIDIOC_PREPARE_BUF,&b,"PREPARE_BUF");
 }
}
static void ctrl(unsigned id,int value){struct v4l2_control c={.id=id,.value=value};call(VIDIOC_S_CTRL,&c,"S_CTRL");}
#include "nereid-capture.h"
static void signal_stop(int sig){(void)sig;stopping=1;}
static int transfer(int fd, void *data, size_t n, int writing, int idle){
 unsigned char *p=data;double deadline=now()+10;
 while(n && !stopping){
  struct pollfd f={.fd=fd,.events=writing?POLLOUT:POLLIN};
  int rc=poll(&f,1,100);
  if(rc<0){if(errno==EINTR)continue;return 0;}
  if(!rc){if(!idle&&now()>deadline)return 0;continue;}
  ssize_t k=writing?write(fd,p,n):read(fd,p,n);
  if(k<=0){if(k<0&&(errno==EINTR||errno==EAGAIN))continue;return 0;}
  p+=k;n-=k;idle=0;
 }
 return n==0;
}
static void cleanup(void){
 if(streaming){
  struct v4l2_encoder_cmd flush={.cmd=4,.flags=3};
  int flushed=0;
  if(ioctl(dev,VIDIOC_ENCODER_CMD,&flush)==0){
   double deadline=now()+3;
   while(now()<deadline){struct pollfd p={.fd=dev,.events=POLLPRI};poll(&p,1,100);struct v4l2_event e={0};if(ioctl(dev,VIDIOC_DQEVENT,&e)==0&&e.type==V4L2_EVENT_PRIVATE_START+0x1001){flushed=1;break;}}
  }
  fprintf(stderr,"Venus flush_done=%d\n",flushed);
  ioctl(dev,VIDIOC_STREAMOFF,&in.type);ioctl(dev,VIDIOC_STREAMOFF,&cap.type);
 }
 if(dev>=0)close(dev);
 for(unsigned k=0;k<2;k++){struct queue *q=k?&cap:&in;for(unsigned i=0;i<q->n;i++)for(unsigned j=0;j<q->np;j++){struct mem *m=&q->m[i][j];if(m->ptr&&m->ptr!=MAP_FAILED)munmap(m->ptr,m->len);if(m->len)close(m->fd);}}
 if(ion>=0)close(ion);
 capture_close();
}
static void init(unsigned bitrate){
 dev=open("/dev/video33",O_RDWR|O_NONBLOCK);if(dev<0)fail("open encoder");
 ion=open("/dev/ion",O_RDWR);if(ion<0)fail("open ion");
 format(V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE,V4L2_PIX_FMT_H264);format(V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE,V4L2_PIX_FMT_NV12);
 ctrl(V4L2_CID_MPEG_VIDEO_BITRATE,bitrate);
 ctrl(V4L2_CID_MPEG_VIDEO_H264_PROFILE,V4L2_MPEG_VIDEO_H264_PROFILE_BASELINE);
 ctrl(V4L2_CID_MPEG_VIDEO_H264_LEVEL,V4L2_MPEG_VIDEO_H264_LEVEL_4_2);
 ctrl(V4L2_CID_MPEG_VIDEO_BITRATE_MODE,V4L2_MPEG_VIDEO_BITRATE_MODE_CBR);
 ctrl(V4L2_CID_MPEG_VIDEO_H264_ENTROPY_MODE,V4L2_MPEG_VIDEO_H264_ENTROPY_MODE_CAVLC);
 ctrl(V4L2_CID_MPEG_VIDEO_HEADER_MODE,2); // downstream: headers with each I frame
 ctrl((V4L2_CTRL_CLASS_MPEG|0x2000)+5,1); // every I frame is IDR
 ctrl((V4L2_CTRL_CLASS_MPEG|0x2000)+6,34); // 35-frame GOP
 struct v4l2_streamparm sp={.type=V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE};sp.parm.output.timeperframe.numerator=1;sp.parm.output.timeperframe.denominator=fps;call(VIDIOC_S_PARM,&sp,"S_PARM");
 struct v4l2_event_subscription sub={.type=V4L2_EVENT_PRIVATE_START+0x1001};call(VIDIOC_SUBSCRIBE_EVENT,&sub,"SUBSCRIBE flush");
 allocq(&in);allocq(&cap);prepare(&in);prepare(&cap);
 for(unsigned i=0;i<cap.n;i++)put(&cap,i,0);
}
static unsigned char clamp(int x){return x<0?0:x>255?255:x;}
static void convert(const unsigned char *rgb){
 unsigned stride=(w+127)&~127u,lines=(h+31)&~31u;struct mem *m=&in.m[0][0];
 if((uint64_t)stride*(lines+((h/2+15)&~15u))>m->len)exit(3);
 memset(m->ptr,16,stride*lines);memset(m->ptr+stride*lines,128,m->len-stride*lines);
 for(unsigned y=0;y<h;y+=2)for(unsigned x=0;x<w;x+=2){
  int rs=0,gs=0,bs=0;
  for(unsigned yy=0;yy<2;yy++)for(unsigned xx=0;xx<2;xx++){
   const unsigned char *p=rgb+((y+yy)*w+x+xx)*4;int b=p[0],g=p[1],r=p[2];
   m->ptr[(y+yy)*stride+x+xx]=clamp(((66*r+129*g+25*b+128)>>8)+16);
   rs+=r;gs+=g;bs+=b;
  }
  int r=(rs+2)/4,g=(gs+2)/4,b=(bs+2)/4;unsigned off=stride*lines+(y/2)*stride+x;
  m->ptr[off]=clamp(((-38*r-74*g+112*b+128)>>8)+128);
  m->ptr[off+1]=clamp(((112*r-94*g-18*b+128)>>8)+128);
 }
}
int main(void){
 signal(SIGTERM,signal_stop);signal(SIGINT,signal_stop);signal(SIGHUP,signal_stop);signal(SIGPIPE,SIG_IGN);
 atexit(cleanup);capture_start();
 unsigned seq=0,bitrate=0;unsigned char *pixels=NULL;
 unsigned char *encoded=malloc(4*1024*1024);if(!encoded)fail("malloc");
 while(!stopping){
  uint32_t request[2];if(!transfer(0,request,sizeof(request),0,1))break;
  if(request[0]>1||request[1]<500000||request[1]>100000000){fprintf(stderr,"Invalid encoder request\n");return 2;}
  double started=now();pixels=capture_frame();if(!pixels)break;
  double captured=now();
  if(!seq){bitrate=request[1];init(bitrate);}
  if(bitrate!=request[1]){bitrate=request[1];ctrl(V4L2_CID_MPEG_VIDEO_BITRATE,bitrate);}
  if(seq&&(request[0]||seq%35==0))ctrl((V4L2_CTRL_CLASS_MPEG|0x2000)+8,1);
  double converting=now();convert(pixels);double submitted=now();put(&in,0,seq);
  if(!streaming){streaming=1;call(VIDIOC_STREAMON,&in.type,"STREAMON input");call(VIDIOC_STREAMON,&cap.type,"STREAMON capture");}
  unsigned used=0,flags=0;int returned=0,video=0;double deadline=now()+3;
  while((!returned||!video)&&!stopping&&now()<deadline){
   struct pollfd pfd={.fd=dev,.events=POLLIN|POLLOUT|POLLPRI};poll(&pfd,1,20);
   struct queue *qs[2]={&cap,&in};
   for(unsigned k=0;k<2;k++){struct queue *q=qs[k];for(;;){
    struct v4l2_plane p[VIDEO_MAX_PLANES]={0};struct v4l2_buffer b={.type=q->type,.memory=V4L2_MEMORY_USERPTR,.length=q->np,.m.planes=p};
    if(ioctl(dev,VIDIOC_DQBUF,&b)<0){if(errno==EAGAIN||errno==EINTR)break;fail("DQBUF");}
    if(b.index>=q->n)return 4;
    if(k==0){unsigned n=p[0].bytesused,off=p[0].data_offset;if(n>q->m[b.index][0].len||off>n||used+n-off>4*1024*1024)return 4;
     memcpy(encoded+used,q->m[b.index][0].ptr+off,n-off);used+=n-off;flags|=b.flags;
     // An access unit must contain an IDR or non-IDR slice, not only SPS/PPS.
     for(unsigned j=0;j+4<used;j++)if(encoded[j]==0&&encoded[j+1]==0&&encoded[j+2]==1){unsigned t=encoded[j+3]&31;if(t==1||t==5)video=1;}
     put(q,b.index,0);
    }else returned=1;
   }}
  }
  if(stopping)break;
  if(!returned||!video){fprintf(stderr,"Venus frame timeout\n");return 1;}
  double finished=now();uint32_t hdr[10]={used,(flags&V4L2_BUF_FLAG_KEYFRAME)?1:0,w,h,(captured-started)*1000000,(submitted-converting)*1000000,(finished-submitted)*1000000,seq,(uint32_t)presentation_us,(uint32_t)(presentation_us>>32)};
  if(!transfer(1,hdr,sizeof(hdr),1,0)||!transfer(1,encoded,used,1,0))break;
  if(seq%175==0)fprintf(stderr,"Venus frame=%u bytes=%u capture_us=%u convert_us=%u encode_us=%u\n",seq,used,hdr[4],hdr[5],hdr[6]);
  seq++;
 }
 free(encoded);return 0;
}
