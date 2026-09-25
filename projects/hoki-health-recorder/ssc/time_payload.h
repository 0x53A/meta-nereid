#ifndef HOKI_SSC_TIME_PAYLOAD_H
#define HOKI_SSC_TIME_PAYLOAD_H
#include <stdint.h>
#include <stddef.h>
/* Stock fsl_cfg::update_timestamp: user-info field8, seconds1,
 * milliseconds2 (zero), timezone3 (signed integer hours east of UTC).
 * Reject fractional-hour offsets instead of silently copying stock truncation.
 * Does not set the system clock or send any request. */
static size_t ssc_time_varint(unsigned char *out,uint64_t value) {
 size_t n=0;
 do { out[n]=(unsigned char)(value&127);value>>=7;
      if(value)out[n]|=128;n++; } while(value);
 return n;
}
static int ssc_time_payload(unsigned char *out,size_t capacity,uint64_t seconds,
                            int offset_seconds,size_t *length) {
 if(!out || !length || seconds<1577836800ULL || seconds>4102444800ULL ||
    offset_seconds < -12*3600 || offset_seconds > 14*3600 || offset_seconds%3600)
  return -1;
 unsigned char nested[32];size_t n=0;
 nested[n++]=8;n+=ssc_time_varint(nested+n,seconds);
 nested[n++]=16;nested[n++]=0;
 nested[n++]=24;
 n+=ssc_time_varint(nested+n,(uint64_t)(int64_t)(offset_seconds/3600));
 if(capacity<n+2)return -1;
 out[0]=0x42;out[1]=(unsigned char)n;
 for(size_t i=0;i<n;i++)out[i+2]=nested[i];
 *length=n+2;return 0;
}
/* GET_MINUTE_LOG (field1=1), timestamp as client-request field2. */
static int ssc_minute_time_payload(unsigned char *out,size_t capacity,uint64_t seconds,
                                   int offset_seconds,size_t *length) {
 unsigned char time_data[32];size_t size=0;
 if(!out || !length || ssc_time_payload(time_data,sizeof time_data,seconds,
                                      offset_seconds,&size) || capacity<size+2)return -1;
 out[0]=8;out[1]=1;out[2]=18;
 for(size_t i=1;i<size;i++)out[i+2]=time_data[i];
 *length=size+2;return 0;
}
#endif
