//! One-shot encrypted PIN transport. Author: Lukas Rieger <code@lukasrieger.com>.
pub mod backend;
mod keymaster;
pub mod protocol;
pub mod secure_container;
pub mod service;
mod sodium;

pub const BUS: &str = "io.Nereid.Auth1";
pub const PATH: &str = "/io/Nereid/Auth1";
pub type Error = Box<dyn std::error::Error + Send + Sync>;

pub struct State {
    pub enrolled: bool,
    pub locked: bool,
    pub busy: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Unlocked,
    Enrolled,
    PinChanged,
    PinCleared,
    StorageProtected,
    Rejected,
    Retry { after_ms: u32 },
    Unavailable,
}
pub struct Attempt {
    owner: String,
    id: Vec<u8>,
    public_key: Vec<u8>,
}
pub struct Client {
    connection: zbus::blocking::Connection,
}
impl Client {
    pub fn system() -> Result<Self, Error> {
        Ok(Self {
            connection: zbus::blocking::connection::Builder::system()?
                .method_timeout(std::time::Duration::from_secs(300))
                .build()?,
        })
    }
    /// A separate connection on a private test bus; production uses system().
    pub fn from_connection(connection: zbus::blocking::Connection) -> Self {
        Self { connection }
    }
    fn proxy<'a>(&'a self, owner: &'a str) -> Result<zbus::blocking::Proxy<'a>, Error> {
        Ok(zbus::blocking::Proxy::new(
            &self.connection,
            owner,
            PATH,
            BUS,
        )?)
    }
    pub fn state(&self) -> Result<State, Error> {
        let (enrolled, locked, busy): (bool, bool, bool) =
            self.proxy(BUS)?.call("GetState", &())?;
        Ok(State {
            enrolled,
            locked,
            busy,
        })
    }
    pub fn begin_attempt(&self) -> Result<Attempt, Error> {
        self.begin("BeginAttempt")
    }
    pub fn begin_management(&self) -> Result<Attempt, Error> {
        self.begin("BeginManagement")
    }
    fn begin(&self, method: &str) -> Result<Attempt, Error> {
        // Pin both halves to this owner. A daemon restart never resubmits a PIN.
        let dbus = zbus::blocking::fdo::DBusProxy::new(&self.connection)?;
        let owner = dbus.get_name_owner(BUS.try_into()?)?.to_string();
        let (id, public_key): (Vec<u8>, Vec<u8>) = self.proxy(&owner)?.call(method, &())?;
        if id.len() != 32 || public_key.len() != 32 {
            return Err("Invalid attempt metadata".into());
        }
        Ok(Attempt {
            owner,
            id,
            public_key,
        })
    }
    pub fn submit_pin(&self, attempt: Attempt, pin: &[u8]) -> Result<Outcome, Error> {
        self.submit(attempt, pin, "SubmitPin")
    }
    /// Explicit Settings enrollment; ordinary PIN verification never enrolls.
    pub fn enroll_pin(&self, attempt: Attempt, pin: &[u8]) -> Result<Outcome, Error> {
        self.submit(attempt, pin, "SubmitEnrollment")
    }
    pub fn change_pin(
        &self,
        attempt: Attempt,
        current: &[u8],
        new: &[u8],
    ) -> Result<Outcome, Error> {
        self.manage(attempt, current, Some(new))
    }
    pub fn clear_pin(&self, attempt: Attempt, current: &[u8]) -> Result<Outcome, Error> {
        self.manage(attempt, current, None)
    }
    fn manage(
        &self,
        attempt: Attempt,
        current: &[u8],
        new: Option<&[u8]>,
    ) -> Result<Outcome, Error> {
        let ciphertext = protocol::seal_management(&attempt.id, &attempt.public_key, current, new)?;
        self.send(attempt, ciphertext, "ManagePin")
    }
    fn submit(&self, attempt: Attempt, pin: &[u8], method: &str) -> Result<Outcome, Error> {
        let ciphertext = protocol::seal(&attempt.id, &attempt.public_key, pin)?;
        self.send(attempt, ciphertext, method)
    }
    fn send(&self, attempt: Attempt, ciphertext: Vec<u8>, method: &str) -> Result<Outcome, Error> {
        let (code, delay): (String, u32) = self
            .proxy(&attempt.owner)?
            .call(method, &(attempt.id, ciphertext))?;
        Ok(match code.as_str() {
            "unlocked" => Outcome::Unlocked,
            "enrolled" => Outcome::Enrolled,
            "pin-changed" => Outcome::PinChanged,
            "pin-cleared" => Outcome::PinCleared,
            "storage-protected" => Outcome::StorageProtected,
            "rejected" => Outcome::Rejected,
            "retry" => Outcome::Retry { after_ms: delay },
            _ => Outcome::Unavailable,
        })
    }
}
