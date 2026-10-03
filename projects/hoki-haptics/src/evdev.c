/* SPDX-License-Identifier: GPL-3.0-only */
/* Use target Linux headers so ff_effect pointers and input_event timestamps
 * have the correct ABI on both ARM32 and desktop previews. */
#include <errno.h>
#include <fcntl.h>
#include <glob.h>
#include <linux/input.h>
#include <stdint.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

int hap_open(void) {
    glob_t paths = {0};
    int result = -ENODEV;
    if (glob("/dev/input/event*", 0, NULL, &paths)) {
        globfree(&paths);
        return result;
    }
    for (size_t i = 0; i < paths.gl_pathc; ++i) {
        int fd = open(paths.gl_pathv[i], O_RDWR | O_CLOEXEC);
        if (fd < 0) {
            if (errno == EACCES) result = -EACCES;
            continue;
        }
        char name[128] = {0};
        unsigned char bits[(FF_MAX + 8) / 8] = {0};
        if (ioctl(fd, EVIOCGNAME(sizeof(name)), name) >= 0 &&
            strcmp(name, "qti-haptics") == 0 &&
            ioctl(fd, EVIOCGBIT(EV_FF, sizeof(bits)), bits) >= 0 &&
            (bits[FF_CONSTANT / 8] & (1 << (FF_CONSTANT % 8))) &&
            (bits[FF_PERIODIC / 8] & (1 << (FF_PERIODIC % 8))) &&
            (bits[FF_CUSTOM / 8] & (1 << (FF_CUSTOM % 8)))) {
            result = fd;
            break;
        }
        close(fd);
    }
    globfree(&paths);
    return result;
}

/* preset=-1: constant pulse. Otherwise the driver's FF_CUSTOM payload is
 * exactly three signed 16-bit words: ID, returned seconds, returned ms.
 * It is a preset selector, NOT an arbitrary waveform sample buffer. */
int hap_upload(int fd, int *id, int preset, int strength, int ms) {
    if (strength < 1 || strength > 100 || preset < -1 || preset > 5 ||
        ms < 20 || ms > 500) return -EINVAL;
    struct ff_effect effect = {0};
    int16_t data[3] = {(int16_t)preset, 0, 0};
    effect.id = *id;
    effect.replay.length = ms;
    if (preset < 0) {
        effect.type = FF_CONSTANT;
        effect.u.constant.level = strength * 32767 / 100;
    } else {
        effect.type = FF_PERIODIC;
        effect.u.periodic.waveform = FF_CUSTOM;
        effect.u.periodic.magnitude = strength * 32767 / 100;
        effect.u.periodic.custom_len = 3;
        effect.u.periodic.custom_data = data;
    }
    if (ioctl(fd, EVIOCSFF, &effect) < 0) return -errno;
    *id = effect.id;
    if (preset < 0) return ms;
    int duration = data[1] * 1000 + data[2];
    return duration > 0 && duration <= 500 ? duration : -ERANGE;
}

int hap_play(int fd, int id, int on) {
    struct input_event event = {0};
    event.type = EV_FF;
    event.code = id;
    event.value = on ? 1 : 0;
    ssize_t result;
    do { result = write(fd, &event, sizeof(event)); } while (result < 0 && errno == EINTR);
    return result == sizeof(event) ? 0 : (result < 0 ? -errno : -EIO);
}

int hap_close(int fd, int id) {
    int result = 0;
    if (id >= 0) {
        result = hap_play(fd, id, 0);
        if (ioctl(fd, EVIOCRMFF, id) < 0 && result == 0) result = -errno;
    }
    if (close(fd) < 0 && result == 0) result = -errno;
    return result;
}
