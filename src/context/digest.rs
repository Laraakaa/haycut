use serde::Serialize;

pub const DIGEST_SCHEMA_VERSION: u16 = 1;

pub fn digest_bytes(artifact_kind: &str, schema_version: u16, bytes: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(format!("haycut:{artifact_kind}:{schema_version}\0").as_bytes());
    hasher.update(bytes);
    hasher.finalize().to_hex().to_string()
}

pub fn digest_json<T: Serialize>(
    artifact_kind: &str,
    schema_version: u16,
    value: &T,
) -> Result<String, serde_json::Error> {
    let canonical_bytes = serde_json::to_vec(value)?;
    Ok(digest_bytes(
        artifact_kind,
        schema_version,
        &canonical_bytes,
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn digests_are_stable_and_domain_separated() {
        let first = digest_bytes("segment", 1, b"same bytes");
        let second = digest_bytes("segment", 1, b"same bytes");

        assert_eq!(first, second);
        assert_ne!(first, digest_bytes("request", 1, b"same bytes"));
        assert_ne!(first, digest_bytes("segment", 2, b"same bytes"));
    }

    #[test]
    fn canonical_json_uses_deterministic_map_ordering() {
        let mut first = BTreeMap::new();
        first.insert("a", 1);
        first.insert("b", 2);
        let mut second = BTreeMap::new();
        second.insert("b", 2);
        second.insert("a", 1);

        assert_eq!(
            digest_json("map", 1, &first).unwrap(),
            digest_json("map", 1, &second).unwrap()
        );
    }
}
