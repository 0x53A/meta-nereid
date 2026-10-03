# UAPI source and license provenance

These headers are copied verbatim from the pinned downstream kernel source in
`kernel-msm-fossil-cw`, so production builds do not include headers from an
external `_Tasks` directory or require the kernel source submodule at build
time:

| Staged file | Source path | License marker in source |
|---|---|---|
| `linux/qseecom.h` | `include/uapi/linux/qseecom.h` | No file-level SPDX/copyright notice is present in this downstream copy. The file is part of the kernel tree governed by its `COPYING` (GPL-2.0); preserve its original text and review license metadata before redistribution. |
| `linux/ion.h` | `include/uapi/linux/ion.h` | Original Google copyright and GPL version 2 notice retained in the file. |
| `linux/mmc/ioctl.h` | `include/uapi/linux/mmc/ioctl.h` | `GPL-2.0 WITH Linux-syscall-note` |
| `linux/major.h` | `include/uapi/linux/major.h` | `GPL-2.0 WITH Linux-syscall-note` |

The cross compiler supplies generic and ARM-specific libc/kernel UAPI headers
such as `linux/types.h` and `asm/ioctl.h`. The compiled sources include this
staged directory first; ABI-size assertions and the reviewed RPMB ioctl value
guard the specific target structures used here.

The HMAC-sharing, RPMB protocol, counter cleanup, listener completion, write
decoder and listener source files in this directory are project-authored copies
from `_Tasks/20261001_PIN_Change_Delete` and
`_Tasks/20260928_RPMB_Readonly_Listener`. Their author attribution remains
Lukas Rieger <code@lukasrieger.com>.
