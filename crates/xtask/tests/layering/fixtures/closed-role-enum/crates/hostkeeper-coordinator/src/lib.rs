//! HostRole in a comment is not code.
pub fn fine() {}

#[cfg(test)]
mod tests {
    enum HostRole {}
}

pub static ROLES: &[RoleName] = &[];
