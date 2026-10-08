use nereid_auth::{device_storage, Error};

fn main() -> Result<(), Error> {
    unsafe {
        libc::umask(0o077);
        if libc::geteuid() != 0
            || libc::prctl(libc::PR_SET_DUMPABLE, 0) != 0
            || libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) != 0
        {
            return Err("Cannot protect device storage key memory".into());
        }
    }
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 2 {
        return Err("Usage: nereid-device-storage provision|open|close".into());
    }
    device_storage::run(&args[1])
}
