/* Copyright 2026 Lukas Rieger <code@lukasrieger.com> */
#include <wayland-client.h>
#include "ext-image-copy-capture-v1-client-protocol.h"
#include "ext-image-capture-source-v1-client-protocol.h"
static struct wl_display *wl;
static struct wl_shm *wlshm;
static struct ext_output_image_capture_source_manager_v1 *source_manager;
static struct ext_image_copy_capture_manager_v1 *copy_manager;
static struct ext_image_copy_capture_session_v1 *session;
static struct wl_output *desktop_output;
static struct wl_buffer *shm_buffer;
static unsigned char *shm_pixels;
static unsigned shm_size;
static uint64_t presentation_us;
static int session_done,session_stopped,frame_done,frame_failed,have_xrgb;
static void output_name(void *d,struct wl_output *o,const char *name){(void)d;if(!strcmp(name,"hoki-desktop"))desktop_output=o;}
static void output_geometry(void *d,struct wl_output *o,int32_t x,int32_t y,int32_t pw,int32_t ph,int32_t s,const char *make,const char *model,int32_t transform){}
static void output_mode(void *d,struct wl_output *o,uint32_t flags,int32_t width,int32_t height,int32_t refresh){}
static void output_done(void *d,struct wl_output *o){}
static void output_scale(void *d,struct wl_output *o,int32_t scale){}
static void output_description(void *d,struct wl_output *o,const char *text){}
static const struct wl_output_listener output_listener={output_geometry,output_mode,output_done,output_scale,output_name,output_description};
static void global(void *d,struct wl_registry *r,uint32_t name,const char *interface,uint32_t version){
 if(!strcmp(interface,"wl_shm"))wlshm=wl_registry_bind(r,name,&wl_shm_interface,1);
 else if(!strcmp(interface,"ext_output_image_capture_source_manager_v1"))source_manager=wl_registry_bind(r,name,&ext_output_image_capture_source_manager_v1_interface,1);
 else if(!strcmp(interface,"ext_image_copy_capture_manager_v1"))copy_manager=wl_registry_bind(r,name,&ext_image_copy_capture_manager_v1_interface,1);
 else if(!strcmp(interface,"wl_output")&&version>=4){struct wl_output *o=wl_registry_bind(r,name,&wl_output_interface,4);wl_output_add_listener(o,&output_listener,NULL);}
}
static void removed(void *d,struct wl_registry *r,uint32_t name){}
static const struct wl_registry_listener registry_listener={global,removed};
static void buffer_size(void *d,struct ext_image_copy_capture_session_v1 *s,uint32_t width,uint32_t height){if(shm_pixels&&(w!=width||h!=height))session_stopped=1;w=width;h=height;}
static void shm_format(void *d,struct ext_image_copy_capture_session_v1 *s,uint32_t format){if(format==WL_SHM_FORMAT_XRGB8888)have_xrgb=1;}
static void dma_device(void *d,struct ext_image_copy_capture_session_v1 *s,struct wl_array *a){}
static void dma_format(void *d,struct ext_image_copy_capture_session_v1 *s,uint32_t f,struct wl_array *a){}
static void done(void *d,struct ext_image_copy_capture_session_v1 *s){session_done=1;}
static void stopped(void *d,struct ext_image_copy_capture_session_v1 *s){session_stopped=1;}
static const struct ext_image_copy_capture_session_v1_listener session_listener={buffer_size,shm_format,dma_device,dma_format,done,stopped};
static void transform(void *d,struct ext_image_copy_capture_frame_v1 *f,uint32_t transform){if(transform!=WL_OUTPUT_TRANSFORM_NORMAL)frame_failed=1;}
static void damage(void *d,struct ext_image_copy_capture_frame_v1 *f,int32_t x,int32_t y,int32_t width,int32_t height){}
static void presentation(void *d,struct ext_image_copy_capture_frame_v1 *f,uint32_t hi,uint32_t lo,uint32_t ns){presentation_us=(((uint64_t)hi<<32)|lo)*1000000+ns/1000;}
static void ready(void *d,struct ext_image_copy_capture_frame_v1 *f){frame_done=1;}
static void failed(void *d,struct ext_image_copy_capture_frame_v1 *f,uint32_t reason){fprintf(stderr,"Capture frame failed: reason=%u\n",reason);frame_failed=1;frame_done=1;}
static const struct ext_image_copy_capture_frame_v1_listener frame_listener={transform,damage,presentation,ready,failed};
static int events(void){
 if(wl_display_dispatch_pending(wl)<0)return 0;
 while(wl_display_prepare_read(wl)!=0)if(wl_display_dispatch_pending(wl)<0)return 0;
 if(wl_display_flush(wl)<0&&errno!=EAGAIN){wl_display_cancel_read(wl);return 0;}
 struct pollfd p={.fd=wl_display_get_fd(wl),.events=POLLIN};int rc=poll(&p,1,100);
 if(rc<=0){wl_display_cancel_read(wl);return rc==0||errno==EINTR;}
 if(wl_display_read_events(wl)<0)return 0;
 return wl_display_dispatch_pending(wl)>=0;
}
static void capture_start(void){
 wl=wl_display_connect("/run/user/1000/wayland-0");if(!wl)fail("Wayland connect");
 struct wl_registry *registry=wl_display_get_registry(wl);wl_registry_add_listener(registry,&registry_listener,NULL);
 if(wl_display_roundtrip(wl)<0||wl_display_roundtrip(wl)<0||!wlshm||!source_manager||!copy_manager||!desktop_output){fprintf(stderr,"Missing desktop capture globals\n");exit(1);}
 struct ext_image_capture_source_v1 *source=ext_output_image_capture_source_manager_v1_create_source(source_manager,desktop_output);
 session=ext_image_copy_capture_manager_v1_create_session(copy_manager,source,1);
 ext_image_copy_capture_session_v1_add_listener(session,&session_listener,NULL);ext_image_capture_source_v1_destroy(source);
 double deadline=now()+5;
 while(!session_done&&!session_stopped&&!stopping&&now()<deadline)if(!events())fail("Wayland events");
 if(!session_done||session_stopped||!have_xrgb||w<96||h<64||w>1920||h>1088||w%2||h%2){fprintf(stderr,"Unsupported capture constraints\n");exit(1);}
 shm_size=w*h*4;int fd=memfd_create("pict-venus-shm",MFD_CLOEXEC);if(fd<0||ftruncate(fd,shm_size)<0)fail("capture memfd");
 shm_pixels=mmap(NULL,shm_size,PROT_READ|PROT_WRITE,MAP_SHARED,fd,0);if(shm_pixels==MAP_FAILED)fail("capture mmap");
 struct wl_shm_pool *pool=wl_shm_create_pool(wlshm,fd,shm_size);shm_buffer=wl_shm_pool_create_buffer(pool,0,w,h,w*4,WL_SHM_FORMAT_XRGB8888);wl_shm_pool_destroy(pool);close(fd);
}
static unsigned char *capture_frame(void){
 frame_done=frame_failed=0;
 struct ext_image_copy_capture_frame_v1 *f=ext_image_copy_capture_session_v1_create_frame(session);
 ext_image_copy_capture_frame_v1_add_listener(f,&frame_listener,NULL);
 ext_image_copy_capture_frame_v1_attach_buffer(f,shm_buffer);
 ext_image_copy_capture_frame_v1_damage_buffer(f,0,0,w,h);
 ext_image_copy_capture_frame_v1_capture(f);
 double deadline=now()+5;
 while(!frame_done&&!session_stopped&&!stopping&&now()<deadline)if(!events())break;
 ext_image_copy_capture_frame_v1_destroy(f);
 if(!frame_done||frame_failed||session_stopped||stopping){fprintf(stderr,"Capture ended: done=%d failed=%d session_stopped=%d stopping=%d display_error=%d\n",frame_done,frame_failed,session_stopped,stopping,wl_display_get_error(wl));return NULL;}
 return shm_pixels;
}
static void capture_close(void){
 if(wl)wl_display_disconnect(wl);
 if(shm_pixels&&shm_pixels!=MAP_FAILED)munmap(shm_pixels,shm_size);
}
