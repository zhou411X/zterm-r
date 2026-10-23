//! Passwords remain in the desktop keyring. Only non-secret lookup metadata is passed to SSH.

use crate::model::Server;
use glib::prelude::Cast;
use libsecret::prelude::ItemExt;
use std::collections::HashMap;

fn schema() -> libsecret::Schema {
    libsecret::Schema::new(
        "org.zterm.Server",
        libsecret::SchemaFlags::NONE,
        HashMap::from([("server", libsecret::SchemaAttributeType::String)]),
    )
}

pub fn save_password(id: &str, name: &str, password: &str) -> Result<(), String> {
    libsecret::password_store_sync(
        Some(&schema()),
        HashMap::from([("server", id)]),
        Some(libsecret::COLLECTION_DEFAULT),
        &format!("zTerm · {name}"),
        password,
        gio::Cancellable::NONE,
    )
    .map_err(|e| e.to_string())
}

pub fn delete_password(id: &str) -> Result<(), String> {
    libsecret::password_clear_sync(
        Some(&schema()),
        HashMap::from([("server", id)]),
        gio::Cancellable::NONE,
    )
    .map_err(|e| e.to_string())
}

pub fn lookup_password(id: &str, background: bool) -> Result<Option<String>, String> {
    if !background {
        return libsecret::password_lookup_sync(
            Some(&schema()),
            HashMap::from([("server", id)]),
            gio::Cancellable::NONE,
        )
        .map(|value| value.map(|s| s.to_string()))
        .map_err(|e| e.to_string());
    }
    // LOAD_SECRETS reads only currently unlocked items. UNLOCK must never be set here.
    let items = libsecret::password_search_sync(
        Some(&schema()),
        HashMap::from([("server", id)]),
        libsecret::SearchFlags::LOAD_SECRETS,
        gio::Cancellable::NONE,
    )
    .map_err(|e| e.to_string())?;
    for retrievable in items {
        if let Ok(item) = retrievable.downcast::<libsecret::Item>() {
            if let Some(secret) = item.secret().and_then(|value| value.text()) {
                return Ok(Some(secret.to_string()));
            }
        }
    }
    Ok(None)
}

pub fn ssh_environment(
    id: &str,
    profile: Option<&Server>,
    background: bool,
) -> Vec<(String, String)> {
    let Some(profile) = profile.filter(|profile| profile.password_saved) else {
        return Vec::new();
    };
    let Ok(executable) = std::env::current_exe() else {
        return Vec::new();
    };
    vec![
        (
            "SSH_ASKPASS".into(),
            executable.to_string_lossy().into_owned(),
        ),
        ("SSH_ASKPASS_REQUIRE".into(), "force".into()),
        ("ZTERM_ASKPASS".into(), "1".into()),
        (
            "ZTERM_SERVER_ID".into(),
            profile.credential_id.as_deref().unwrap_or(id).into(),
        ),
        ("ZTERM_SSH_USER".into(), profile.username.clone()),
        ("ZTERM_SSH_HOST".into(), profile.host.clone()),
        (
            "ZTERM_SSH_BACKGROUND".into(),
            if background { "1" } else { "0" }.into(),
        ),
    ]
}

pub fn saved_answer(prompt: &str) -> Result<Option<String>, String> {
    let user = std::env::var("ZTERM_SSH_USER").unwrap_or_default();
    let host = std::env::var("ZTERM_SSH_HOST").unwrap_or_default();
    let id = std::env::var("ZTERM_SERVER_ID").unwrap_or_default();
    if user.is_empty()
        || host.is_empty()
        || id.is_empty()
        || prompt.trim() != format!("{user}@{host}'s password:")
    {
        return Ok(None);
    }
    lookup_password(
        &id,
        std::env::var("ZTERM_SSH_BACKGROUND").as_deref() == Ok("1"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn askpass_environment_has_only_metadata() {
        let profile = Server {
            name: "测试机".into(),
            host: "192.0.2.10".into(),
            username: "user".into(),
            port: 22,
            password_saved: true,
            credential_id: Some("old-id".into()),
        };
        let environment = ssh_environment("new-id", Some(&profile), true);
        assert!(environment.contains(&("ZTERM_SERVER_ID".into(), "old-id".into())));
        assert!(environment.contains(&("ZTERM_SSH_BACKGROUND".into(), "1".into())));
        assert!(environment.contains(&("ZTERM_ASKPASS".into(), "1".into())));
        assert!(ssh_environment(
            "new-id",
            Some(&Server {
                password_saved: false,
                ..profile
            }),
            false
        )
        .is_empty());
    }
}
