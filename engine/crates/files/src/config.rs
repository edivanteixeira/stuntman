//! files/servers.yaml: the file servers partners and the app exchange files on (EDI, NOTFIS,
//! CONEMB, OCOREN…), their users and the failures provoked on them.

use serde::{Deserialize, Serialize};
use stuntman_core::{util, yamlfile, ApiError, Ctx};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Servers {
    pub servers: Vec<Server>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Server {
    /// edi-acme: also the folder files/<name>/.
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
    pub users: Vec<User>,
    /// "" | login_refused | disk_full | permission_denied | drop_mid | slow | host_key_changed
    #[serde(skip_serializing_if = "String::is_empty")]
    pub provoke: String,
    /// Folders made on creation (/in, /out…).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct User {
    pub user: String,
    /// secret://name or a synthetic literal; empty: any password except "wrong".
    #[serde(skip_serializing_if = "String::is_empty")]
    pub password: String,
    /// Read only: uploads, deletes and renames answer "permission denied".
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
}

pub const PROVOKE: [(&str, &str); 6] = [("login_refused", "530 on every login"), ("disk_full", "552 on uploads"), ("permission_denied", "550 on reads and writes"),
    ("drop_mid", "the transfer breaks after 4 KB"), ("slow", "20 KB/s"), ("host_key_changed", "SFTP shows another host key")];

pub fn path(ctx: &Ctx) -> std::path::PathBuf {
    ctx.path("files/servers.yaml")
}

pub fn load(ctx: &Ctx) -> Servers {
    yamlfile::load(&path(ctx)).unwrap_or_default()
}

pub fn save(ctx: &Ctx, s: &Servers) -> Result<(), ApiError> {
    yamlfile::save(&path(ctx), s)
}

pub fn root(ctx: &Ctx, server: &str) -> std::path::PathBuf {
    ctx.path(&format!("files/{server}"))
}

/// The server and user a login names: "user" (the first server that has it) or "user@server".
pub fn login(ctx: &Ctx, name: &str) -> Option<(Server, User)> {
    let s = load(ctx);
    let (u, srv) = name.split_once('@').map(|(a, b)| (a.to_string(), Some(b.to_string()))).unwrap_or((name.to_string(), None));
    s.servers.into_iter().filter(|x| srv.as_ref().map(|n| &x.name == n).unwrap_or(true)).find_map(|x| x.users.iter().find(|y| y.user == u).cloned().map(|y| (x.clone(), y)))
}

pub fn password_ok(ctx: &Ctx, u: &User, pw: &str) -> bool {
    if u.password.is_empty() { !pw.is_empty() && pw != "wrong" } else { util::resolve_secret(&u.password, &ctx.data_dir) == pw }
}

/// A path the client gave, inside the server's folder (never above it).
pub fn resolve(cwd: &str, p: &str) -> String {
    let joined = if p.starts_with('/') { p.to_string() } else { format!("{}/{p}", cwd.trim_end_matches('/')) };
    let mut out: Vec<&str> = vec![];
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    format!("/{}", out.join("/"))
}

pub fn disk(ctx: &Ctx, server: &str, virt: &str) -> std::path::PathBuf {
    root(ctx, server).join(virt.trim_start_matches('/'))
}
