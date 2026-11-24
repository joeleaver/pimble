//! The shape of an agent's key, which both ends read: the server to find the
//! agent a key names, the app to find whose entry an app's config holds
//! (docs/AGENTS_CONTRACT.md section 5).

use uuid::Uuid;

/// What every agent key starts with, so a key is recognisable for what it is
/// (and never mistaken for a JWT or the server's static token).
pub const AGENT_KEY_PREFIX: &str = "pmbl_agent_";

/// A key for agent `id` with `secret`: the prefix, the id (so a key names its
/// agent), an underscore and the secret.
pub fn agent_key(id: Uuid, secret: &str) -> String {
    format!("{AGENT_KEY_PREFIX}{}_{secret}", id.simple())
}

/// The agent id a key names, if it is shaped like an agent key. This says
/// whose key it claims to be, not that it is that agent's: only the server,
/// which keeps the key's hash, can say that.
pub fn agent_id_of_key(key: &str) -> Option<Uuid> {
    let rest = key.strip_prefix(AGENT_KEY_PREFIX)?;
    let (id, _) = rest.split_once('_')?;
    Uuid::parse_str(id).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_names_its_agent() {
        let id = Uuid::new_v4();
        let key = agent_key(id, "s3cr_et");
        assert_eq!(agent_id_of_key(&key), Some(id));
        assert_eq!(agent_id_of_key("pmbl_agent_nothex_x"), None);
        assert_eq!(agent_id_of_key("eyJhbGciOi"), None);
    }
}
