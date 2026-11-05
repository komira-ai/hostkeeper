//! HostRole in a comment is not code.
pub fn fine() {}

#[cfg(test)]
mod tests {
    enum HostRole {}
}

pub static ROLES: &[RoleName] = &[];

/* HostRole in a block comment, /* nested: enum NodeRole */ still a comment. */
pub const NOTE: &str = "HostRole in a string, and \" enum NodeRole { static X: RoleName";
pub const RAW: &str = r#"static X: RoleName = y; "HostRole""#;
pub const QUOTE: char = '"';
