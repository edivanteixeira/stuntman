//! SFTP: SSH with password logins and the sftp subsystem over the same folders as FTP. The host
//! key is kept (files/ssh_host_ed25519) so clients that pinned it keep trusting it; the
//! host_key_changed provocation shows another one.

use std::{collections::HashMap, sync::Arc, time::Duration};

use russh::{
    keys::PrivateKey,
    server::{Auth, Msg, Session},
    Channel, ChannelId,
};
use russh_sftp::protocol::{Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version};
use stuntman_core::{env::Envs, Ctx};
use tokio::sync::Mutex;

use crate::{config, store};

fn random_key() -> PrivateKey {
    let seed: [u8; 32] = rand::random();
    PrivateKey::from(russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&seed))
}

fn host_key(ctx: &Ctx) -> PrivateKey {
    let p = ctx.path("files/ssh_host_ed25519");
    if let Ok(t) = std::fs::read_to_string(&p) {
        if let Ok(k) = PrivateKey::from_openssh(&t) {
            return k;
        }
    }
    let k = random_key();
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    if let Ok(t) = k.to_openssh(russh::keys::ssh_key::LineEnding::LF) {
        let _ = std::fs::write(&p, t.as_bytes());
    }
    k
}

struct Ssh {
    ctx: Ctx,
    login: Option<(config::Server, config::User)>,
    channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
}

impl russh::server::Handler for Ssh {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        match config::login(&self.ctx, user) {
            Some((srv, _)) if srv.provoke == "login_refused" => {
                store::log(&self.ctx, &srv.name, user, "SFTP", "login", "", 0, false, "login refused (provoked)").await;
                Ok(Auth::reject())
            }
            Some((srv, u)) if config::password_ok(&self.ctx, &u, password) => {
                let _ = std::fs::create_dir_all(config::root(&self.ctx, &srv.name));
                store::log(&self.ctx, &srv.name, user, "SFTP", "login", "", 0, true, "logged in").await;
                self.login = Some((srv, u));
                Ok(Auth::Accept)
            }
            Some((srv, _)) => {
                store::log(&self.ctx, &srv.name, user, "SFTP", "login", "", 0, false, "wrong password").await;
                Ok(Auth::reject())
            }
            None => Ok(Auth::reject()),
        }
    }

    async fn channel_open_session(&mut self, channel: Channel<Msg>, reply: russh::server::ChannelOpenHandle, _session: &mut Session) -> Result<(), Self::Error> {
        self.channels.lock().await.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(&mut self, id: ChannelId, name: &str, session: &mut Session) -> Result<(), Self::Error> {
        let ch = self.channels.lock().await.remove(&id);
        match (name, ch, self.login.clone()) {
            ("sftp", Some(ch), Some((srv, u))) => {
                session.channel_success(id)?;
                let h = Sftp { ctx: self.ctx.clone(), server: srv.name.clone(), user: u.user.clone(), read_only: u.read_only, files: HashMap::new(), dirs: HashMap::new(), next: 0 };
                tokio::spawn(russh_sftp::server::run(ch.into_stream(), h));
            }
            _ => session.channel_failure(id)?,
        }
        Ok(())
    }
}

struct Open {
    path: String,
    file: tokio::fs::File,
    written: u64,
    read: u64,
    write: bool,
    failed: bool,
}

struct Sftp {
    ctx: Ctx,
    server: String,
    user: String,
    read_only: bool,
    files: HashMap<String, Open>,
    dirs: HashMap<String, (String, bool)>,
    next: u64,
}

fn ok(id: u32) -> Status {
    Status { id, status_code: StatusCode::Ok, error_message: "Ok".into(), language_tag: "en-US".into() }
}

impl Sftp {
    fn provoke(&self) -> String {
        config::load(&self.ctx).servers.into_iter().find(|s| s.name == self.server).map(|s| s.provoke).unwrap_or_default()
    }
    fn disk(&self, p: &str) -> std::path::PathBuf {
        config::disk(&self.ctx, &self.server, &config::resolve("/", p))
    }
    fn handle(&mut self) -> String {
        self.next += 1;
        format!("h{}", self.next)
    }
}

fn attrs(m: &std::fs::Metadata) -> FileAttributes {
    FileAttributes::from(m)
}

impl russh_sftp::server::Handler for Sftp {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(&mut self, _version: u32, _ext: HashMap<String, String>) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let p = config::resolve("/", if path.is_empty() || path == "." { "/" } else { &path });
        Ok(Name { id, files: vec![File::dummy(&p)] })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        std::fs::metadata(self.disk(&path)).map(|m| Attrs { id, attrs: attrs(&m) }).map_err(|_| StatusCode::NoSuchFile)
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.stat(id, path).await
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let o = self.files.get(&handle).ok_or(StatusCode::Failure)?;
        o.file.metadata().await.map(|m| Attrs { id, attrs: attrs(&m) }).map_err(|_| StatusCode::Failure)
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        if self.provoke() == "permission_denied" {
            return Err(StatusCode::PermissionDenied);
        }
        if !self.disk(&path).is_dir() {
            return Err(StatusCode::NoSuchFile);
        }
        let h = self.handle();
        self.dirs.insert(h.clone(), (path, false));
        Ok(Handle { id, handle: h })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let (path, done) = self.dirs.get(&handle).cloned().ok_or(StatusCode::Failure)?;
        if done {
            return Err(StatusCode::Eof);
        }
        self.dirs.insert(handle, (path.clone(), true));
        let files = std::fs::read_dir(self.disk(&path)).map(|r| r.flatten().filter_map(|e| Some(File::new(e.file_name().to_string_lossy().to_string(), attrs(&e.metadata().ok()?)))).collect()).unwrap_or_default();
        Ok(Name { id, files })
    }

    async fn open(&mut self, id: u32, filename: String, pflags: OpenFlags, _a: FileAttributes) -> Result<Handle, Self::Error> {
        let write = pflags.contains(OpenFlags::WRITE) || pflags.contains(OpenFlags::CREATE) || pflags.contains(OpenFlags::APPEND);
        let prov = self.provoke();
        let path = config::resolve("/", &filename);
        if prov == "permission_denied" || (write && self.read_only) {
            store::log(&self.ctx, &self.server, &self.user, "SFTP", if write { "put" } else { "get" }, &path, 0, false, if self.read_only { "read-only user" } else { "permission denied (provoked)" }).await;
            return Err(StatusCode::PermissionDenied);
        }
        if write && prov == "disk_full" {
            store::log(&self.ctx, &self.server, &self.user, "SFTP", "put", &path, 0, false, "no space left (provoked)").await;
            return Err(StatusCode::Failure);
        }
        let d = self.disk(&filename);
        // Folders exist only when set_file_server declares them or mkdir makes them: SSH_FX_NO_SUCH_FILE, as OpenSSH.
        if !d.parent().map(|p| p.is_dir()).unwrap_or(false) {
            store::log(&self.ctx, &self.server, &self.user, "SFTP", if write { "put" } else { "get" }, &path, 0, false, "no such folder").await;
            return Err(StatusCode::NoSuchFile);
        }
        let mut o = tokio::fs::OpenOptions::new();
        o.read(pflags.contains(OpenFlags::READ)).write(write).create(pflags.contains(OpenFlags::CREATE)).truncate(pflags.contains(OpenFlags::TRUNCATE)).append(pflags.contains(OpenFlags::APPEND));
        let file = o.open(&d).await.map_err(|_| StatusCode::NoSuchFile)?;
        let h = self.handle();
        self.files.insert(h.clone(), Open { path, file, written: 0, read: 0, write, failed: false });
        Ok(Handle { id, handle: h })
    }

    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<Data, Self::Error> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let prov = self.provoke();
        let o = self.files.get_mut(&handle).ok_or(StatusCode::Failure)?;
        if prov == "drop_mid" && o.read >= 4096 {
            o.failed = true;
            return Err(StatusCode::ConnectionLost);
        }
        if prov == "slow" {
            tokio::time::sleep(Duration::from_millis(100 * (len as u64 / 2048).max(1))).await;
        }
        o.file.seek(std::io::SeekFrom::Start(offset)).await.map_err(|_| StatusCode::Failure)?;
        let mut buf = vec![0u8; len as usize];
        let n = o.file.read(&mut buf).await.map_err(|_| StatusCode::Failure)?;
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        buf.truncate(n);
        o.read += n as u64;
        Ok(Data { id, data: buf })
    }

    async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> Result<Status, Self::Error> {
        use tokio::io::{AsyncSeekExt, AsyncWriteExt};
        let prov = self.provoke();
        let o = self.files.get_mut(&handle).ok_or(StatusCode::Failure)?;
        if prov == "drop_mid" && o.written >= 4096 {
            o.failed = true;
            return Err(StatusCode::ConnectionLost);
        }
        if prov == "slow" {
            tokio::time::sleep(Duration::from_millis(100 * (data.len() as u64 / 2048).max(1))).await;
        }
        o.file.seek(std::io::SeekFrom::Start(offset)).await.map_err(|_| StatusCode::Failure)?;
        o.file.write_all(&data).await.map_err(|_| StatusCode::Failure)?;
        o.written += data.len() as u64;
        Ok(ok(id))
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        if let Some(o) = self.files.remove(&handle) {
            let detail = if o.failed { "dropped mid-transfer (provoked)" } else if o.write { "uploaded" } else { "downloaded" };
            store::log(&self.ctx, &self.server, &self.user, "SFTP", if o.write { "put" } else { "get" }, &o.path, if o.write { o.written } else { o.read }, !o.failed, detail).await;
        }
        self.dirs.remove(&handle);
        Ok(ok(id))
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        if self.read_only || self.provoke() == "permission_denied" {
            return Err(StatusCode::PermissionDenied);
        }
        std::fs::remove_file(self.disk(&filename)).map_err(|_| StatusCode::NoSuchFile)?;
        store::log(&self.ctx, &self.server, &self.user, "SFTP", "delete", &config::resolve("/", &filename), 0, true, "deleted").await;
        Ok(ok(id))
    }

    async fn mkdir(&mut self, id: u32, path: String, _a: FileAttributes) -> Result<Status, Self::Error> {
        if self.read_only || self.provoke() == "permission_denied" {
            return Err(StatusCode::PermissionDenied);
        }
        std::fs::create_dir_all(self.disk(&path)).map_err(|_| StatusCode::Failure)?;
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        if self.read_only {
            return Err(StatusCode::PermissionDenied);
        }
        std::fs::remove_dir(self.disk(&path)).map_err(|_| StatusCode::Failure)?;
        Ok(ok(id))
    }

    async fn rename(&mut self, id: u32, oldpath: String, newpath: String) -> Result<Status, Self::Error> {
        if self.read_only || self.provoke() == "permission_denied" {
            return Err(StatusCode::PermissionDenied);
        }
        std::fs::rename(self.disk(&oldpath), self.disk(&newpath)).map_err(|_| StatusCode::Failure)?;
        store::log(&self.ctx, &self.server, &self.user, "SFTP", "rename", &format!("{} → {}", config::resolve("/", &oldpath), config::resolve("/", &newpath)), 0, true, "renamed").await;
        Ok(ok(id))
    }

    async fn setstat(&mut self, id: u32, _p: String, _a: FileAttributes) -> Result<Status, Self::Error> {
        Ok(ok(id))
    }

    async fn fsetstat(&mut self, id: u32, _h: String, _a: FileAttributes) -> Result<Status, Self::Error> {
        Ok(ok(id))
    }
}

pub fn serve(envs: Envs, port: u16) {
    tokio::spawn(async move {
        let Ok(l) = tokio::net::TcpListener::bind(("0.0.0.0", port)).await else {
            eprintln!("SFTP: port {port} is taken");
            return;
        };
        loop {
            let Ok((sock, _addr)) = l.accept().await else { continue };
            let Some(ctx) = envs.default_env() else { continue };
            tokio::spawn(async move {
                let changed = config::load(&ctx).servers.iter().any(|s| s.provoke == "host_key_changed");
                let key = if changed { random_key() } else { host_key(&ctx) };
                let cfg = Arc::new(russh::server::Config { keys: vec![key], auth_rejection_time: Duration::from_millis(200), auth_rejection_time_initial: Some(Duration::from_millis(0)), inactivity_timeout: Some(Duration::from_secs(600)), ..Default::default() });
                let h = Ssh { ctx, login: None, channels: Arc::new(Mutex::new(HashMap::new())) };
                if let Ok(s) = russh::server::run_stream(cfg, sock, h).await {
                    let _ = s.await;
                }
            });
        }
    });
}
