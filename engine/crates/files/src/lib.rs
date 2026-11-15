//! Files (A15): the FTP, FTPS and SFTP servers partners and the app drop files on (EDI: NOTFIS,
//! CONEMB, OCOREN, DOCCOB…). Each server is a folder; you put files "as the partner would" and see
//! every login and transfer; logins can be refused, disks fill, transfers drop halfway or crawl.

pub mod config;
pub mod ftp;
pub mod sftp;
pub mod store;
pub mod tools;

use stuntman_core::{Module, Tool};

pub struct Files {
    pub ftp_port: u16,
    pub sftp_port: u16,
}

impl Module for Files {
    fn id(&self) -> &'static str {
        "files"
    }

    fn migrations(&self) -> &'static str {
        store::SQL
    }

    fn tools(&self) -> Vec<Tool> {
        tools::all()
    }

    fn listen(&self, envs: stuntman_core::env::Envs) {
        ftp::serve(envs.clone(), self.ftp_port);
        sftp::serve(envs, self.sftp_port);
    }
}
