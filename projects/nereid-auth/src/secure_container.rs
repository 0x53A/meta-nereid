// Copyright (C) 2026 Lukas Rieger <code@lukasrieger.com>
//! Explicit LUKS2 storage provisioning and mount lifecycle for nereid-auth.
//!
//! This module never persists the Keymaster passphrase. It is borrowed only
//! while writing a one-shot pipe; the caller owns and erases the key buffer.

use std::{
    ffi::{CString, OsStr, OsString},
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
        unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    },
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};
pub const KEY_BYTES: usize = 32;
pub const DEFAULT_IMAGE_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_IMAGE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const DEFAULT_STATE_DIRECTORY: &str = "/var/lib/nereid-auth";
pub const DEFAULT_MOUNT_POINT: &str = "/mnt/secure";
pub const IMAGE_NAME: &str = "secure.luks";
pub const PROVISION_MARKER: &str = "secure.luks.state";
pub const MAPPING_NAME: &str = "nereid-secure";

const MIN_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const SERVICE_PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";
const MARKER_PENDING: &[u8] = b"NEREID-SECURE-PENDING-1\n";
const MARKER_READY: &[u8] = b"NEREID-SECURE-READY-1\n";

/// Paths and size for the one persistent LUKS2 image.
///
/// Executable paths should be absolute in the deployed service. The default
/// names are suitable only when the service provides a trusted fixed `PATH`.
#[derive(Clone, Debug)]
pub struct SecureContainerConfig {
    pub state_directory: PathBuf,
    pub mount_point: PathBuf,
    pub image_bytes: u64,
    pub cryptsetup: PathBuf,
    pub mkfs_ext4: PathBuf,
    pub mount: PathBuf,
    pub umount: PathBuf,
    pub chmod: PathBuf,
    pub command_timeout: Duration,
}

impl Default for SecureContainerConfig {
    fn default() -> Self {
        Self {
            state_directory: PathBuf::from(DEFAULT_STATE_DIRECTORY),
            mount_point: PathBuf::from(DEFAULT_MOUNT_POINT),
            image_bytes: DEFAULT_IMAGE_BYTES,
            cryptsetup: PathBuf::from("cryptsetup"),
            mkfs_ext4: PathBuf::from("mkfs.ext4"),
            mount: PathBuf::from("mount"),
            umount: PathBuf::from("umount"),
            chmod: PathBuf::from("chmod"),
            command_timeout: COMMAND_TIMEOUT,
        }
    }
}

#[derive(Debug)]
pub enum SecureContainerError {
    Io(io::Error),
    InvalidConfiguration,
    UnsafeStateDirectory,
    UnsafeMountPoint,
    UnsafeImage,
    UnsafeProvisionMarker,
    OperationLockPoisoned,
    ImageAlreadyExists,
    ImageMissing,
    RecoveryRequired,
    MountPointNotEmpty,
    CommandFailed(&'static str),
    CommandTimedOut(&'static str),
    UnmountOrMountStateUnverified,
    MappingCloseFailed,
    MountCleanupFailed,
    ProvisionCleanupFailed,
    SynchronizationFailed,
}

impl fmt::Display for SecureContainerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(_) => f.write_str("secure container I/O failed"),
            Self::InvalidConfiguration => f.write_str("invalid secure container configuration"),
            Self::UnsafeStateDirectory => {
                f.write_str("secure container state directory must be root-owned mode 0700")
            }
            Self::UnsafeMountPoint => {
                f.write_str("secure container mount point is unsafe or not root-owned")
            }
            Self::UnsafeImage => {
                f.write_str("secure container image must be a root-owned regular file mode 0600")
            }
            Self::UnsafeProvisionMarker => f.write_str("provision marker is unsafe"),
            Self::OperationLockPoisoned => {
                f.write_str("secure container operation lock is poisoned")
            }
            Self::ImageAlreadyExists => {
                f.write_str("secure container image already exists; refusing to reformat it")
            }
            Self::ImageMissing => f.write_str("secure container image is not provisioned"),
            Self::RecoveryRequired => {
                f.write_str("secure container provisioning is incomplete; recovery is required")
            }
            Self::MountPointNotEmpty => {
                f.write_str("secure container mount point is not empty")
            }
            Self::CommandFailed(action) => write!(f, "secure container {action} command failed"),
            Self::CommandTimedOut(action) => {
                write!(f, "secure container {action} command timed out")
            }
            Self::UnmountOrMountStateUnverified => f.write_str(
                "mount state could not be safely unmounted or was busy; mapping close was not attempted and key eviction is not confirmed",
            ),
            Self::MappingCloseFailed => f.write_str(
                "unmount succeeded but cryptsetup close failed; the mapping may remain active and key eviction is not confirmed",
            ),
            Self::MountCleanupFailed => f.write_str(
                "mount setup failed and cryptsetup cleanup failed; the mapping may remain active and key eviction is not confirmed",
            ),
            Self::ProvisionCleanupFailed => f.write_str(
                "filesystem provisioning failed and cryptsetup cleanup failed; the mapping may remain active and key eviction is not confirmed",
            ),
            Self::SynchronizationFailed => {
                f.write_str("secure container state could not be synchronized")
            }
        }
    }
}

impl std::error::Error for SecureContainerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for SecureContainerError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug)]
enum RunnerError {
    Io(io::Error),
    Failed,
    TimedOut,
}

trait CommandRunner: Send + Sync {
    fn mapping_present(&self, mapping: &Path) -> Result<bool, RunnerError>;
    fn mounted_source(&self, mount_point: &Path) -> Result<Option<PathBuf>, RunnerError>;

    fn run(
        &self,
        program: &Path,
        args: &[OsString],
        stdin_secret: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<(), RunnerError>;
}

struct ProcessRunner;

impl CommandRunner for ProcessRunner {
    fn mapping_present(&self, mapping: &Path) -> Result<bool, RunnerError> {
        match fs::symlink_metadata(mapping) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(RunnerError::Io(error)),
        }
    }

    fn mounted_source(&self, mount_point: &Path) -> Result<Option<PathBuf>, RunnerError> {
        mount_source_at(mount_point).map_err(RunnerError::Io)
    }

    fn run(
        &self,
        program: &Path,
        args: &[OsString],
        stdin_secret: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<(), RunnerError> {
        let mut command = Command::new(program);
        command
            .args(args)
            .env_clear()
            .env("PATH", SERVICE_PATH)
            .stdin(if stdin_secret.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let mut child = command.spawn().map_err(RunnerError::Io)?;
        if let Some(secret) = stdin_secret {
            let Some(mut stdin) = child.stdin.take() else {
                stop_child(&mut child);
                return Err(RunnerError::Io(io::Error::other(
                    "cryptsetup stdin pipe was unavailable",
                )));
            };
            if let Err(error) = stdin.write_all(secret) {
                drop(stdin);
                stop_child(&mut child);
                return Err(RunnerError::Io(error));
            }
            // EOF is part of the key-file framing. Do not append a newline.
            drop(stdin);
        }

        let deadline = Instant::now() + timeout;
        loop {
            let status = match child.try_wait() {
                Ok(status) => status,
                Err(error) => {
                    stop_child(&mut child);
                    return Err(RunnerError::Io(error));
                }
            };
            match status {
                Some(status) if status.success() => return Ok(()),
                Some(_) => return Err(RunnerError::Failed),
                None if Instant::now() >= deadline => {
                    stop_child(&mut child);
                    return Err(RunnerError::TimedOut);
                }
                None => thread::sleep(POLL_INTERVAL),
            }
        }
    }
}

fn stop_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Narrow lifecycle API for the Keymaster-derived 32-byte LUKS key.
///
/// `provision` is explicit and never reformats an existing image. `open`
/// validates the existing persistent image before mapping and mounting it.
/// `close` uses only ordinary unmount/close operations; a busy mount leaves the
/// mapping alone and is reported to the caller.
pub struct SecureContainer {
    config: SecureContainerConfig,
    runner: Arc<dyn CommandRunner>,
    expected_uid: u32,
    operation_lock: Mutex<()>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MarkerState {
    Missing,
    Pending,
    Ready,
}

impl SecureContainer {
    pub fn new(config: SecureContainerConfig) -> Result<Self, SecureContainerError> {
        Self::build(config, Arc::new(ProcessRunner), 0)
    }

    pub fn system_default() -> Result<Self, SecureContainerError> {
        Self::new(SecureContainerConfig::default())
    }

    /// Read-only startup validation, before any hardware key operation.
    pub fn preflight(&self) -> Result<(), SecureContainerError> {
        let _guard = self.lock_operation()?;
        self.open_state_directory()?;
        self.open_mount_point(true)?;
        self.ensure_inactive()?;
        for program in [
            &self.config.cryptsetup,
            &self.config.mkfs_ext4,
            &self.config.mount,
            &self.config.umount,
            &self.config.chmod,
        ] {
            let candidates = if program.is_absolute() {
                vec![program.clone()]
            } else {
                ["/usr/sbin", "/usr/bin", "/sbin", "/bin"]
                    .iter()
                    .map(|directory| Path::new(directory).join(program))
                    .collect()
            };
            let safe = candidates.iter().any(|path| {
                fs::metadata(path).is_ok_and(|metadata| {
                    metadata.is_file()
                        && metadata.uid() == 0
                        && metadata.mode() & 0o022 == 0
                        && metadata.mode() & 0o111 != 0
                })
            });
            if !safe {
                return Err(SecureContainerError::CommandFailed(
                    "required executable preflight",
                ));
            }
        }
        Ok(())
    }

    fn build(
        config: SecureContainerConfig,
        runner: Arc<dyn CommandRunner>,
        expected_uid: u32,
    ) -> Result<Self, SecureContainerError> {
        if !clean_absolute_path(&config.state_directory)
            || !clean_absolute_path(&config.mount_point)
            || config.image_bytes < MIN_IMAGE_BYTES
            || config.image_bytes > MAX_IMAGE_BYTES
            || !config.image_bytes.is_multiple_of(4096)
            || config.command_timeout.is_zero()
            || config.command_timeout > Duration::from_secs(300)
            || [
                &config.cryptsetup,
                &config.mkfs_ext4,
                &config.mount,
                &config.umount,
                &config.chmod,
            ]
            .iter()
            .any(|path| path.as_os_str().is_empty() || path.as_os_str().as_bytes().contains(&0))
        {
            return Err(SecureContainerError::InvalidConfiguration);
        }
        Ok(Self {
            config,
            runner,
            expected_uid,
            operation_lock: Mutex::new(()),
        })
    }

    /// Create a new image and format it as LUKS2 containing ext4.
    ///
    /// A durable pending marker is written before image creation/formatting.
    /// It changes to a ready marker only after filesystem creation and mapping
    /// close succeed. Any partial provision therefore blocks automatic open or
    /// reformat on a later call.
    pub fn provision(&self, key: &[u8; KEY_BYTES]) -> Result<(), SecureContainerError> {
        let _guard = self.lock_operation()?;
        self.provision_with_key(key)
    }

    /// Open the existing LUKS2 image and mount its ext4 filesystem.
    pub fn open(&self, key: &[u8; KEY_BYTES]) -> Result<(), SecureContainerError> {
        let _guard = self.lock_operation()?;
        self.open_with_key(key)
    }

    /// Unmount and then close the mapping. No force or lazy unmount is used.
    pub fn close(&self) -> Result<(), SecureContainerError> {
        let _guard = self.lock_operation()?;
        let mountpoint = self.open_mount_point(false)?;
        drop(mountpoint);

        let source = self
            .runner
            .mounted_source(&self.config.mount_point)
            .map_err(|_| SecureContainerError::CommandFailed("mount state check"))?;
        if !source
            .as_deref()
            .is_some_and(|source| same_block_device(source, &mapper_path()))
        {
            return Err(SecureContainerError::UnmountOrMountStateUnverified);
        }

        if self
            .run(
                &self.config.umount,
                &[self.config.mount_point.as_os_str().to_owned()],
                None,
                "unmount",
            )
            .is_err()
        {
            return Err(SecureContainerError::UnmountOrMountStateUnverified);
        }
        self.close_mapping()
    }

    fn provision_with_key(&self, key: &[u8; KEY_BYTES]) -> Result<(), SecureContainerError> {
        let directory = self.open_state_directory()?;
        self.ensure_inactive()?;
        match self.marker_state(&directory)? {
            MarkerState::Pending => return Err(SecureContainerError::RecoveryRequired),
            MarkerState::Ready => return Err(SecureContainerError::ImageAlreadyExists),
            MarkerState::Missing => {}
        }
        if self.image_exists(&directory)? {
            return Err(SecureContainerError::ImageAlreadyExists);
        }

        let mut marker = create_private_file_at(&directory, PROVISION_MARKER, self.expected_uid)?;
        marker.write_all(MARKER_PENDING)?;
        marker
            .sync_all()
            .map_err(|_| SecureContainerError::SynchronizationFailed)?;
        sync_directory(&directory)?;

        // Exclusive creation plus O_NOFOLLOW prevents clobbering or following
        // an existing path, even if it changed after the checks above.
        let image = create_private_file_at(&directory, IMAGE_NAME, self.expected_uid)?;
        image
            .set_len(self.config.image_bytes)
            .map_err(|_| SecureContainerError::SynchronizationFailed)?;
        image
            .sync_all()
            .map_err(|_| SecureContainerError::SynchronizationFailed)?;
        sync_directory(&directory)?;
        self.verify_image_file(&image)?;

        let image_path = self.image_path();
        let format_args = [
            OsString::from("luksFormat"),
            OsString::from("--type=luks2"),
            OsString::from("--batch-mode"),
            OsString::from("--force-password"),
            OsString::from("--cipher=aes-xts-plain64"),
            OsString::from("--key-size=512"),
            OsString::from("--sector-size=512"),
            OsString::from("--pbkdf=pbkdf2"),
            OsString::from("--pbkdf-force-iterations=100000"),
            OsString::from("--key-file=-"),
            OsString::from(format!("--keyfile-size={KEY_BYTES}")),
            image_path.as_os_str().to_owned(),
        ];
        self.run(
            &self.config.cryptsetup,
            &format_args,
            Some(key),
            "LUKS2 format",
        )?;
        image
            .sync_all()
            .map_err(|_| SecureContainerError::SynchronizationFailed)?;

        self.open_mapping(key)?;
        let mkfs_args = [
            OsString::from("-F"),
            OsString::from("-m"),
            OsString::from("0"),
            OsString::from("-O"),
            OsString::from("^orphan_file,^metadata_csum_seed"),
            OsString::from("-L"),
            OsString::from(MAPPING_NAME),
            mapper_path().as_os_str().to_owned(),
        ];
        if self
            .run(&self.config.mkfs_ext4, &mkfs_args, None, "ext4 format")
            .is_err()
        {
            return match self.close_mapping() {
                Ok(()) => Err(SecureContainerError::CommandFailed("ext4 format")),
                Err(_) => Err(SecureContainerError::ProvisionCleanupFailed),
            };
        }
        if image.sync_all().is_err() {
            return match self.close_mapping() {
                Ok(()) => Err(SecureContainerError::SynchronizationFailed),
                Err(_) => Err(SecureContainerError::ProvisionCleanupFailed),
            };
        }
        self.close_mapping()
            .map_err(|_| SecureContainerError::ProvisionCleanupFailed)?;
        image
            .sync_all()
            .map_err(|_| SecureContainerError::SynchronizationFailed)?;

        marker
            .set_len(0)
            .map_err(|_| SecureContainerError::SynchronizationFailed)?;
        marker
            .seek(SeekFrom::Start(0))
            .map_err(|_| SecureContainerError::SynchronizationFailed)?;
        marker
            .write_all(MARKER_READY)
            .map_err(|_| SecureContainerError::SynchronizationFailed)?;
        marker
            .sync_all()
            .map_err(|_| SecureContainerError::SynchronizationFailed)?;
        Ok(())
    }

    fn open_with_key(&self, key: &[u8; KEY_BYTES]) -> Result<(), SecureContainerError> {
        let directory = self.open_state_directory()?;
        match self.marker_state(&directory)? {
            MarkerState::Ready => {}
            MarkerState::Pending => return Err(SecureContainerError::RecoveryRequired),
            MarkerState::Missing => {
                return if self.image_exists(&directory)? {
                    Err(SecureContainerError::RecoveryRequired)
                } else {
                    Err(SecureContainerError::ImageMissing)
                };
            }
        }
        let image = open_private_file_at(&directory, IMAGE_NAME, self.expected_uid)?
            .ok_or(SecureContainerError::ImageMissing)?;
        self.verify_image_file(&image)?;
        drop(image);
        self.open_mount_point(true)?;
        self.ensure_inactive()?;

        self.open_mapping(key)?;
        let mount_args = [
            OsString::from("-t"),
            OsString::from("ext4"),
            OsString::from("-o"),
            OsString::from("nosuid,nodev"),
            mapper_path().as_os_str().to_owned(),
            self.config.mount_point.as_os_str().to_owned(),
        ];
        if self
            .run(&self.config.mount, &mount_args, None, "mount")
            .is_err()
        {
            return match self.close_mapping() {
                Ok(()) => Err(SecureContainerError::CommandFailed("mount")),
                Err(_) => Err(SecureContainerError::MountCleanupFailed),
            };
        }

        // Keep the mounted root accessible only to the privileged service.
        let chmod_args = [
            OsString::from("0700"),
            self.config.mount_point.as_os_str().to_owned(),
        ];
        if self
            .run(&self.config.chmod, &chmod_args, None, "mount permissions")
            .is_err()
        {
            let unmount = self.run(
                &self.config.umount,
                &[self.config.mount_point.as_os_str().to_owned()],
                None,
                "unmount after setup failure",
            );
            if unmount.is_err() || self.close_mapping().is_err() {
                return Err(SecureContainerError::MountCleanupFailed);
            }
            return Err(SecureContainerError::CommandFailed("mount permissions"));
        }
        Ok(())
    }

    fn open_mapping(&self, key: &[u8; KEY_BYTES]) -> Result<(), SecureContainerError> {
        let args = [
            OsString::from("open"),
            OsString::from("--type=luks2"),
            OsString::from("--disable-keyring"),
            OsString::from("--key-file=-"),
            OsString::from(format!("--keyfile-size={KEY_BYTES}")),
            self.image_path().as_os_str().to_owned(),
            OsString::from(MAPPING_NAME),
        ];
        self.run(&self.config.cryptsetup, &args, Some(key), "LUKS2 open")
    }

    fn ensure_inactive(&self) -> Result<(), SecureContainerError> {
        let mapping_present = self
            .runner
            .mapping_present(&mapper_path())
            .map_err(|_| SecureContainerError::CommandFailed("mapping state check"))?;
        let mounted = self
            .runner
            .mounted_source(&self.config.mount_point)
            .map_err(|_| SecureContainerError::CommandFailed("mount state check"))?;
        if mapping_present || mounted.is_some() {
            return Err(SecureContainerError::RecoveryRequired);
        }
        Ok(())
    }

    fn close_mapping(&self) -> Result<(), SecureContainerError> {
        let args = [OsString::from("close"), OsString::from(MAPPING_NAME)];
        self.run(&self.config.cryptsetup, &args, None, "LUKS2 close")
            .map_err(|_| SecureContainerError::MappingCloseFailed)
    }

    fn run(
        &self,
        program: &Path,
        args: &[OsString],
        secret: Option<&[u8]>,
        action: &'static str,
    ) -> Result<(), SecureContainerError> {
        match self
            .runner
            .run(program, args, secret, self.config.command_timeout)
        {
            Ok(()) => Ok(()),
            Err(RunnerError::TimedOut) => Err(SecureContainerError::CommandTimedOut(action)),
            Err(RunnerError::Failed) => Err(SecureContainerError::CommandFailed(action)),
            Err(RunnerError::Io(error)) => Err(SecureContainerError::Io(error)),
        }
    }

    fn lock_operation(&self) -> Result<MutexGuard<'_, ()>, SecureContainerError> {
        self.operation_lock
            .lock()
            .map_err(|_| SecureContainerError::OperationLockPoisoned)
    }

    fn image_path(&self) -> PathBuf {
        self.config.state_directory.join(IMAGE_NAME)
    }

    fn open_state_directory(&self) -> Result<File, SecureContainerError> {
        let directory = open_directory(&self.config.state_directory)
            .map_err(|_| SecureContainerError::UnsafeStateDirectory)?;
        verify_directory(&directory, self.expected_uid, 0o700)
            .map_err(|_| SecureContainerError::UnsafeStateDirectory)?;
        Ok(directory)
    }

    fn open_mount_point(&self, require_empty: bool) -> Result<File, SecureContainerError> {
        let directory = open_directory(&self.config.mount_point)
            .map_err(|_| SecureContainerError::UnsafeMountPoint)?;
        verify_directory(&directory, self.expected_uid, 0o700)
            .map_err(|_| SecureContainerError::UnsafeMountPoint)?;
        if require_empty {
            let mut entries = fs::read_dir(&self.config.mount_point)
                .map_err(|_| SecureContainerError::UnsafeMountPoint)?;
            if entries.next().transpose()?.is_some() {
                return Err(SecureContainerError::MountPointNotEmpty);
            }
        }
        Ok(directory)
    }

    fn marker_state(&self, directory: &File) -> Result<MarkerState, SecureContainerError> {
        let Some(mut marker) = open_private_file_at(directory, PROVISION_MARKER, self.expected_uid)
            .map_err(|_| SecureContainerError::UnsafeProvisionMarker)?
        else {
            return Ok(MarkerState::Missing);
        };
        let metadata = marker
            .metadata()
            .map_err(|_| SecureContainerError::UnsafeProvisionMarker)?;
        if metadata.len() > MARKER_PENDING.len() as u64 {
            return Err(SecureContainerError::UnsafeProvisionMarker);
        }
        let mut contents = Vec::with_capacity(metadata.len() as usize);
        marker
            .read_to_end(&mut contents)
            .map_err(|_| SecureContainerError::UnsafeProvisionMarker)?;
        if contents == MARKER_PENDING {
            Ok(MarkerState::Pending)
        } else if contents == MARKER_READY {
            Ok(MarkerState::Ready)
        } else {
            Err(SecureContainerError::UnsafeProvisionMarker)
        }
    }

    fn image_exists(&self, directory: &File) -> Result<bool, SecureContainerError> {
        match open_private_file_at(directory, IMAGE_NAME, self.expected_uid) {
            Ok(Some(_)) => Ok(true),
            Ok(None) => Ok(false),
            Err(_) => Err(SecureContainerError::UnsafeImage),
        }
    }

    fn verify_image_file(&self, image: &File) -> Result<(), SecureContainerError> {
        let metadata = image.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != self.expected_uid
            || metadata.mode() & 0o7777 != 0o600
            || metadata.nlink() != 1
            || metadata.len() != self.config.image_bytes
        {
            return Err(SecureContainerError::UnsafeImage);
        }
        Ok(())
    }
}

fn mapper_path() -> PathBuf {
    PathBuf::from(format!("/dev/mapper/{MAPPING_NAME}"))
}

fn same_block_device(source: &Path, expected: &Path) -> bool {
    if source == expected {
        return true;
    }
    // mount may canonicalize /dev/mapper/name to /dev/dm-N. Compare block
    // device identity before unmounting instead of relying on that spelling.
    match (fs::metadata(source), fs::metadata(expected)) {
        (Ok(a), Ok(b)) => {
            a.file_type().is_block_device()
                && b.file_type().is_block_device()
                && a.rdev() == b.rdev()
        }
        _ => false,
    }
}

fn mount_source_at(mount_point: &Path) -> io::Result<Option<PathBuf>> {
    let mountinfo = fs::read("/proc/self/mountinfo")?;
    parse_mountinfo_source(&mountinfo, mount_point)
}

fn parse_mountinfo_source(mountinfo: &[u8], mount_point: &Path) -> io::Result<Option<PathBuf>> {
    let wanted = mount_point.as_os_str().as_bytes();
    let mut found = None;
    for line in mountinfo.split(|byte| *byte == b'\n') {
        let Some(separator) = line.windows(3).position(|part| part == b" - ") else {
            continue;
        };
        let left = fields(&line[..separator]);
        let right = fields(&line[separator + 3..]);
        if left.len() < 5 || right.len() < 2 {
            continue;
        }
        let Some(path) = unescape_mount_field(left[4]) else {
            continue;
        };
        if path == wanted {
            let source = unescape_mount_field(right[1]).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid mount source")
            })?;
            if found.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "stacked mount point is ambiguous",
                ));
            }
            found = Some(PathBuf::from(OsStr::from_bytes(&source)));
        }
    }
    Ok(found)
}

fn fields(bytes: &[u8]) -> Vec<&[u8]> {
    bytes
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty())
        .collect()
}

fn unescape_mount_field(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let octal = bytes.get(index + 1..index + 4)?;
        if !octal.iter().all(|byte| (b'0'..=b'7').contains(byte)) {
            return None;
        }
        let value = (octal[0] - b'0') * 64 + (octal[1] - b'0') * 8 + (octal[2] - b'0');
        decoded.push(value);
        index += 4;
    }
    Some(decoded)
}

fn clean_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}

fn open_directory(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

fn verify_directory(file: &File, uid: u32, mode: u32) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o7777 != mode {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe directory",
        ));
    }
    Ok(())
}

fn openat_file(directory: &File, name: &str, flags: i32, mode: u32) -> io::Result<File> {
    let name = CString::new(name).expect("fixed file name has no NUL");
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags, mode) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new owned file descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn open_private_file_at(
    directory: &File,
    name: &str,
    uid: u32,
) -> Result<Option<File>, SecureContainerError> {
    match openat_file(
        directory,
        name,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0,
    ) {
        Ok(file) => {
            verify_private_file(&file, uid)?;
            Ok(Some(file))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(SecureContainerError::Io(error)),
    }
}

fn create_private_file_at(
    directory: &File,
    name: &str,
    uid: u32,
) -> Result<File, SecureContainerError> {
    let file = openat_file(
        directory,
        name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0o600,
    )?;
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    verify_private_file(&file, uid)?;
    Ok(file)
}

fn verify_private_file(file: &File, uid: u32) -> Result<(), SecureContainerError> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(SecureContainerError::UnsafeImage);
    }
    Ok(())
}

fn sync_directory(directory: &File) -> Result<(), SecureContainerError> {
    directory
        .sync_all()
        .map_err(|_| SecureContainerError::SynchronizationFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::{HashMap, VecDeque},
        ffi::OsStr,
        os::unix::fs::PermissionsExt,
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc,
        },
    };

    #[derive(Clone, Debug)]
    struct Call {
        program: PathBuf,
        args: Vec<OsString>,
        secret_length: Option<usize>,
        secret_matches_expected: bool,
    }

    #[derive(Default)]
    struct FakeRunner {
        calls: Mutex<Vec<Call>>,
        outcomes: Mutex<VecDeque<(&'static str, bool)>>,
        expected_key: Mutex<Option<[u8; KEY_BYTES]>>,
        mapping_active: Mutex<bool>,
        mounts: Mutex<HashMap<PathBuf, PathBuf>>,
    }

    impl FakeRunner {
        fn with_expected_key(key: [u8; KEY_BYTES]) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                outcomes: Mutex::new(VecDeque::new()),
                expected_key: Mutex::new(Some(key)),
                mapping_active: Mutex::new(false),
                mounts: Mutex::new(HashMap::new()),
            }
        }

        fn fail_on(&self, action: &'static str) {
            self.outcomes.lock().unwrap().push_back((action, false));
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }

        fn set_mount(&self, target: PathBuf, source: PathBuf) {
            self.mounts.lock().unwrap().insert(target, source);
        }

        fn set_mapping_active(&self, active: bool) {
            *self.mapping_active.lock().unwrap() = active;
        }
    }

    impl CommandRunner for FakeRunner {
        fn mapping_present(&self, _mapping: &Path) -> Result<bool, RunnerError> {
            Ok(*self.mapping_active.lock().unwrap())
        }

        fn mounted_source(&self, mount_point: &Path) -> Result<Option<PathBuf>, RunnerError> {
            Ok(self.mounts.lock().unwrap().get(mount_point).cloned())
        }

        fn run(
            &self,
            program: &Path,
            args: &[OsString],
            stdin_secret: Option<&[u8]>,
            _timeout: Duration,
        ) -> Result<(), RunnerError> {
            let action = if program.file_name() == Some(OsStr::new("cryptsetup")) {
                args.first()
                    .and_then(|arg| arg.to_str())
                    .unwrap_or("cryptsetup")
            } else {
                program
                    .file_name()
                    .and_then(OsStr::to_str)
                    .unwrap_or("command")
            };
            let expected = *self.expected_key.lock().unwrap();
            self.calls.lock().unwrap().push(Call {
                program: program.to_owned(),
                args: args.to_vec(),
                secret_length: stdin_secret.map(<[u8]>::len),
                secret_matches_expected: stdin_secret.is_some_and(|secret| {
                    expected
                        .as_ref()
                        .is_some_and(|expected| secret == expected.as_slice())
                }),
            });
            let failed = if self
                .outcomes
                .lock()
                .unwrap()
                .front()
                .is_some_and(|(expected_action, _)| *expected_action == action)
            {
                let (_, success) = self.outcomes.lock().unwrap().pop_front().unwrap();
                !success
            } else {
                false
            };
            if failed {
                return Err(RunnerError::Failed);
            }
            match action {
                "open" => *self.mapping_active.lock().unwrap() = true,
                "close" => *self.mapping_active.lock().unwrap() = false,
                "mount" if args.len() >= 2 => {
                    let source = PathBuf::from(&args[args.len() - 2]);
                    let target = PathBuf::from(&args[args.len() - 1]);
                    self.set_mount(target, source);
                }
                "umount" if !args.is_empty() => {
                    self.mounts.lock().unwrap().remove(&PathBuf::from(&args[0]));
                }
                _ => {}
            }
            Ok(())
        }
    }

    struct TestTree(PathBuf);

    impl TestTree {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "nereid-secure-container-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            fs::create_dir(path.join("state")).unwrap();
            fs::set_permissions(path.join("state"), fs::Permissions::from_mode(0o700)).unwrap();
            fs::create_dir(path.join("mount")).unwrap();
            fs::set_permissions(path.join("mount"), fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }

        fn config(&self) -> SecureContainerConfig {
            let root = self.0.clone();
            SecureContainerConfig {
                state_directory: root.join("state"),
                mount_point: root.join("mount"),
                image_bytes: MIN_IMAGE_BYTES,
                cryptsetup: PathBuf::from("cryptsetup"),
                mkfs_ext4: PathBuf::from("mkfs.ext4"),
                mount: PathBuf::from("mount"),
                umount: PathBuf::from("umount"),
                chmod: PathBuf::from("chmod"),
                command_timeout: Duration::from_secs(1),
            }
        }

        fn state_path(&self) -> PathBuf {
            self.0.join("state")
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn test_container(config: SecureContainerConfig, runner: Arc<FakeRunner>) -> SecureContainer {
        let uid = unsafe { libc::geteuid() };
        SecureContainer::build(config, runner, uid).unwrap()
    }

    fn action(call: &Call) -> String {
        if call.program.file_name() == Some(OsStr::new("cryptsetup")) {
            call.args[0].to_string_lossy().into_owned()
        } else {
            call.program
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        }
    }

    #[test]
    fn provision_uses_one_shot_binary_key_and_never_reformats() {
        let tree = TestTree::new();
        let key = [0xA5; KEY_BYTES];
        let runner = Arc::new(FakeRunner::with_expected_key(key));
        let container = test_container(tree.config(), runner.clone());
        let supplied_key = key;

        container.provision(&supplied_key).unwrap();

        assert_eq!(supplied_key, key);
        let calls = runner.calls();
        assert_eq!(
            calls.iter().map(action).collect::<Vec<_>>(),
            ["luksFormat", "open", "mkfs.ext4", "close"]
        );
        for call in calls
            .iter()
            .filter(|call| action(call) == "luksFormat" || action(call) == "open")
        {
            assert_eq!(call.secret_length, Some(KEY_BYTES));
            assert!(call.secret_matches_expected);
            assert!(call.args.iter().any(|arg| arg == "--key-file=-"));
            assert!(call
                .args
                .iter()
                .any(|arg| arg == &OsString::from(format!("--keyfile-size={KEY_BYTES}"))));
        }
        assert!(calls[0]
            .args
            .iter()
            .any(|arg| arg == "--cipher=aes-xts-plain64"));
        assert!(calls[0].args.iter().any(|arg| arg == "--key-size=512"));
        assert!(calls[0].args.iter().any(|arg| arg == "--sector-size=512"));
        assert!(calls[0].args.iter().any(|arg| arg == "--pbkdf=pbkdf2"));
        assert!(calls[0]
            .args
            .iter()
            .any(|arg| arg == "--pbkdf-force-iterations=100000"));
        assert!(calls[2]
            .args
            .iter()
            .any(|arg| { arg == "^orphan_file,^metadata_csum_seed" }));
        assert!(calls.iter().all(|call| call
            .args
            .iter()
            .all(|arg| { arg.as_os_str().as_bytes() != key.as_slice() })));

        let image = tree.state_path().join(IMAGE_NAME);
        let metadata = fs::symlink_metadata(&image).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(metadata.len(), MIN_IMAGE_BYTES);
        assert_eq!(
            fs::read(tree.state_path().join(PROVISION_MARKER)).unwrap(),
            MARKER_READY
        );

        let next_key = key;
        assert!(matches!(
            container.provision(&next_key),
            Err(SecureContainerError::ImageAlreadyExists)
        ));
        assert_eq!(runner.calls().len(), calls.len());
    }

    #[test]
    fn existing_image_is_never_touched_by_provision() {
        let tree = TestTree::new();
        let image = tree.state_path().join(IMAGE_NAME);
        fs::write(&image, b"do not overwrite").unwrap();
        fs::set_permissions(&image, fs::Permissions::from_mode(0o600)).unwrap();
        let runner = Arc::new(FakeRunner::default());
        let container = test_container(tree.config(), runner.clone());
        let key = [0x33; KEY_BYTES];

        assert!(matches!(
            container.provision(&key),
            Err(SecureContainerError::ImageAlreadyExists)
        ));
        assert_eq!(key, [0x33; KEY_BYTES]);
        assert_eq!(fs::read(image).unwrap(), b"do not overwrite");
        assert!(runner.calls().is_empty());
        assert!(!tree.state_path().join(PROVISION_MARKER).exists());
    }

    #[test]
    fn partial_provision_leaves_marker_and_blocks_reformat_or_open() {
        let tree = TestTree::new();
        let runner = Arc::new(FakeRunner::with_expected_key([0x62; KEY_BYTES]));
        runner.fail_on("luksFormat");
        let container = test_container(tree.config(), runner.clone());
        let key = [0x62; KEY_BYTES];

        assert!(matches!(
            container.provision(&key),
            Err(SecureContainerError::CommandFailed("LUKS2 format"))
        ));
        assert_eq!(key, [0x62; KEY_BYTES]);
        assert!(tree.state_path().join(PROVISION_MARKER).is_file());
        assert!(tree.state_path().join(IMAGE_NAME).is_file());

        let initial_calls = runner.calls().len();
        let retry_key = [0x62; KEY_BYTES];
        assert!(matches!(
            container.provision(&retry_key),
            Err(SecureContainerError::RecoveryRequired)
        ));
        let open_key = [0x62; KEY_BYTES];
        assert!(matches!(
            container.open(&open_key),
            Err(SecureContainerError::RecoveryRequired)
        ));
        assert_eq!(runner.calls().len(), initial_calls);
    }

    #[test]
    fn open_mounts_hardened_ext4_and_close_unmounts_before_mapping_close() {
        let tree = TestTree::new();
        let image = create_test_image(&tree);
        drop(image);
        let key = [0x79; KEY_BYTES];
        let runner = Arc::new(FakeRunner::with_expected_key(key));
        let container = test_container(tree.config(), runner.clone());
        let supplied_key = key;

        container.open(&supplied_key).unwrap();
        assert_eq!(supplied_key, key);
        let open_calls = runner.calls();
        assert_eq!(
            open_calls.iter().map(action).collect::<Vec<_>>(),
            ["open", "mount", "chmod"]
        );
        assert_eq!(open_calls[0].secret_length, Some(KEY_BYTES));
        assert!(open_calls[0].secret_matches_expected);
        assert!(open_calls[0]
            .args
            .iter()
            .any(|arg| arg == "--disable-keyring"));
        assert!(open_calls[1].args.iter().any(|arg| arg == "nosuid,nodev"));

        container.close().unwrap();
        let calls = runner.calls();
        assert_eq!(
            calls.iter().map(action).collect::<Vec<_>>(),
            ["open", "mount", "chmod", "umount", "close"]
        );
        assert!(calls
            .iter()
            .filter(|call| action(call) == "umount")
            .all(|call| !call.args.iter().any(|arg| arg == "-l" || arg == "-f")));
    }

    #[test]
    fn busy_unmount_surfaces_retained_mapping_without_attempting_close() {
        let tree = TestTree::new();
        create_test_image(&tree);
        let key = [0x91; KEY_BYTES];
        let runner = Arc::new(FakeRunner::with_expected_key(key));
        let container = test_container(tree.config(), runner.clone());
        container.open(&key).unwrap();
        runner.fail_on("umount");

        assert!(matches!(
            container.close(),
            Err(SecureContainerError::UnmountOrMountStateUnverified)
        ));
        let calls = runner.calls();
        assert_eq!(
            calls.iter().map(action).collect::<Vec<_>>(),
            ["open", "mount", "chmod", "umount"]
        );
        assert!(!calls.iter().any(|call| action(call) == "close"));
    }

    #[test]
    fn startup_refuses_existing_mapping_or_mount_without_touching_it() {
        let tree = TestTree::new();
        let key = [0x14; KEY_BYTES];
        let runner = Arc::new(FakeRunner::with_expected_key(key));
        let container = test_container(tree.config(), runner.clone());

        runner.set_mapping_active(true);
        assert!(matches!(
            container.provision(&key),
            Err(SecureContainerError::RecoveryRequired)
        ));
        runner.set_mapping_active(false);
        runner.set_mount(tree.config().mount_point.clone(), mapper_path());
        assert!(matches!(
            container.provision(&key),
            Err(SecureContainerError::RecoveryRequired)
        ));
        assert!(runner.calls().is_empty());
        assert!(!tree.state_path().join(PROVISION_MARKER).exists());
        assert!(!tree.state_path().join(IMAGE_NAME).exists());
    }

    #[test]
    fn close_refuses_to_unmount_a_foreign_source() {
        let tree = TestTree::new();
        let runner = Arc::new(FakeRunner::default());
        runner.set_mount(
            tree.config().mount_point.clone(),
            PathBuf::from("/dev/mapper/foreign"),
        );
        let container = test_container(tree.config(), runner.clone());

        assert!(matches!(
            container.close(),
            Err(SecureContainerError::UnmountOrMountStateUnverified)
        ));
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn mountinfo_parser_decodes_mount_path_escapes_and_checks_device_source() {
        let mount_point = Path::new("/run/test mount");
        let data = b"36 25 0:32 / /run/test\\040mount rw - ext4 /dev/mapper/nereid-secure rw\n";
        assert_eq!(
            parse_mountinfo_source(data, mount_point).unwrap(),
            Some(mapper_path())
        );
        let foreign = b"36 25 0:32 / /run/test\\040mount rw - ext4 /dev/mapper/foreign rw\n";
        assert_ne!(
            parse_mountinfo_source(foreign, mount_point).unwrap(),
            Some(mapper_path())
        );
        let stacked = b"36 25 0:32 / /run/test\\040mount rw - ext4 /dev/mapper/nereid-secure rw\n37 25 0:32 / /run/test\\040mount rw - ext4 /dev/mapper/foreign rw\n";
        assert_eq!(
            parse_mountinfo_source(stacked, mount_point)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    fn create_test_image(tree: &TestTree) -> File {
        let image = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(tree.state_path().join(IMAGE_NAME))
            .unwrap();
        image.set_len(MIN_IMAGE_BYTES).unwrap();
        fs::write(tree.state_path().join(PROVISION_MARKER), MARKER_READY).unwrap();
        fs::set_permissions(
            tree.state_path().join(PROVISION_MARKER),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        image
    }
}
