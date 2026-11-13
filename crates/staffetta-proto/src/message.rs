use std::collections::HashMap;

/// The sender of a message: either a server name, or a nick with
/// optional user/host (`nick[!user][@host]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefix {
    pub name: String,
    pub user: Option<String>,
    pub host: Option<String>,
}

/// A parsed IRC line. Tags are an open key/value map -- no fixed fields --
/// so a new IRCv3 spec never requires touching this struct or the parser,
/// only adding a `Capability` that reads the keys it cares about.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RawMessage {
    pub tags: HashMap<String, Option<String>>,
    pub prefix: Option<Prefix>,
    pub command: String,
    pub params: Vec<String>,
}
