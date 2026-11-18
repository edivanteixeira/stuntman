"""End-to-end check of Files (A15): FTP, explicit FTPS (AUTH TLS, PROT P) and SFTP against the same
folders; a file put "as the partner"; uploads the app makes; logins refused, a full disk,
permission denied, a transfer dropped halfway, a changed SFTP host key; the transfers log.

    pip install paramiko   (for SFTP)
    cargo build -p stuntman && python3 tests/e2e_files.py
"""
import ftplib, io, os, socket, ssl, tempfile, time

from harness import Stuntman, free_port, wait

OCOREN = "000ACME TRANSPORTES      GLOBEX LOGISTICA         151126014200OCO15111\r\n3411234567890123400001501112026014200 ENTREGUE\r\n" * 3


def ftp(s, user="edi", pw="secret", tls=False):
    if tls:
        ctx = ssl.create_default_context(cafile=s.ca)
        ctx.check_hostname = False
        f = ftplib.FTP_TLS(context=ctx, timeout=15)
    else:
        f = ftplib.FTP(timeout=15)
    f.connect("127.0.0.1", s.FTP_PORT)
    f.login(user, pw)
    if tls:
        f.prot_p()
    return f


def main():
    with Stuntman("stuntman-files-") as s:
        pem = s.call("GET", "/apis/test-ca")["pem"]
        fd, s.ca = tempfile.mkstemp(suffix=".pem")
        os.write(fd, pem.encode()); os.close(fd)
        s.call("PUT", "/files/servers/edi-acme", {"title": "edi.acme.example", "users": [{"user": "edi", "password": "secret"}, {"user": "auditor", "read_only": True}], "folders": ["/in/notfis", "/out/ocoren"]})

        # The partner drops a NOTFIS; the app downloads it over FTP.
        s.call("POST", "/files/servers/edi-acme/put", {"path": "/in/notfis/NOTFIS_1042.txt", "text": "000GLOBEX NOTFIS 1042\r\n"})
        f = ftp(s)
        assert f.pwd() == "/"
        f.cwd("/in/notfis")
        assert f.nlst() == ["NOTFIS_1042.txt"], f.nlst()
        buf = io.BytesIO()
        f.retrbinary("RETR NOTFIS_1042.txt", buf.write)
        assert buf.getvalue() == b"000GLOBEX NOTFIS 1042\r\n"
        assert f.size("NOTFIS_1042.txt") == 23
        # The app uploads an OCOREN, renames it, lists.
        f.cwd("/out/ocoren")
        f.storbinary("STOR OCOREN_0142.tmp", io.BytesIO(OCOREN.encode()))
        f.rename("OCOREN_0142.tmp", "OCOREN_0142.txt")
        assert "OCOREN_0142.txt" in " ".join(f.nlst()), f.nlst()
        lines = []
        f.retrlines("LIST", lines.append)
        assert lines[0].startswith("-rw-r--r--") and lines[0].endswith("OCOREN_0142.txt"), lines
        f.quit()
        got = s.call("GET", "/files/servers/edi-acme/wait?path=/out/ocoren/*.txt")
        assert got["size"] == len(OCOREN), got
        assert s.call("GET", "/files/servers/edi-acme/file?path=/out/ocoren/OCOREN_0142.txt")["text"] == OCOREN

        # FTPS: control and data in TLS.
        f = ftp(s, tls=True)
        f.cwd("/out/ocoren")
        f.storbinary("STOR OCOREN_0143.txt", io.BytesIO(b"tls upload"))
        assert "OCOREN_0143.txt" in f.nlst()
        f.quit()

        # A folder nobody declared: 553 on upload (vsftpd), 550 on CWD, and no file made.
        f = ftp(s)
        try:
            f.storbinary("STOR /missing/x.txt", io.BytesIO(b"x")); raise AssertionError("no such folder")
        except ftplib.error_perm as e:
            assert str(e).startswith("553"), e
        try:
            f.cwd("/missing"); raise AssertionError("no such folder")
        except ftplib.error_perm as e:
            assert str(e).startswith("550"), e
        s.call("GET", "/files/servers/edi-acme/file?path=/missing/x.txt", expect=404)
        f.mkd("/out/new")
        f.storbinary("STOR /out/new/x.txt", io.BytesIO(b"x"))

        # Active mode: PORT (ftplib with passive off), EPRT, and 425 when the client is not listening.
        f.set_pasv(False)
        f.storbinary("STOR /out/new/active.txt", io.BytesIO(b"active upload"))
        buf = io.BytesIO()
        f.retrbinary("RETR /out/new/active.txt", buf.write)
        assert buf.getvalue() == b"active upload", buf.getvalue()
        lsn = socket.socket(); lsn.bind(("127.0.0.1", 0)); lsn.listen(1)
        assert f.sendcmd(f"EPRT |1|127.0.0.1|{lsn.getsockname()[1]}|").startswith("200")
        f.putcmd("NLST /out/new")
        assert f.getresp().startswith("150")
        d, _ = lsn.accept()
        names = b""
        while chunk := d.recv(4096):
            names += chunk
        d.close(); lsn.close()
        assert f.getresp().startswith("226") and sorted(names.decode().split()) == ["active.txt", "x.txt"], names
        closed = free_port()
        assert f.sendcmd(f"PORT 127,0,0,1,{closed >> 8},{closed & 255}").startswith("200")
        f.putcmd("STOR /out/new/never.txt")
        assert f.getresp().startswith("150")
        try:
            f.getresp(); raise AssertionError("425")
        except ftplib.error_temp as e:
            assert str(e).startswith("425"), e
        s.call("GET", "/files/servers/edi-acme/file?path=/out/new/never.txt", expect=404)
        try:
            f.sendcmd("PORT 10,1,2,3,4,5"); raise AssertionError("bounce")
        except ftplib.error_perm as e:
            assert str(e).startswith("500"), e
        f.quit()

        # Read-only user, wrong password.
        f = ftp(s, "auditor", "anything")
        try:
            f.storbinary("STOR /out/x.txt", io.BytesIO(b"x")); raise AssertionError("read-only")
        except ftplib.error_perm as e:
            assert str(e).startswith("550"), e
        f.quit()
        try:
            ftp(s, "edi", "wrong"); raise AssertionError("wrong password")
        except ftplib.error_perm as e:
            assert str(e).startswith("530")

        # Provocations.
        s.call("PUT", "/files/servers/edi-acme", {"provoke": "disk_full"})
        f = ftp(s)
        try:
            f.storbinary("STOR /out/full.txt", io.BytesIO(b"x")); raise AssertionError("disk full")
        except ftplib.error_temp as e:
            raise AssertionError(e)
        except ftplib.error_perm as e:
            assert str(e).startswith("552"), e
        f.close()
        s.call("PUT", "/files/servers/edi-acme", {"provoke": "drop_mid"})
        s.call("POST", "/files/servers/edi-acme/put", {"path": "/in/big.bin", "text": "x" * 20000})
        f = ftp(s)
        buf = io.BytesIO()
        try:
            f.retrbinary("RETR /in/big.bin", buf.write); raise AssertionError("dropped")
        except (ftplib.error_temp, EOFError, ConnectionError) as e:
            assert len(buf.getvalue()) == 4096, len(buf.getvalue())
        f.close()
        s.call("PUT", "/files/servers/edi-acme", {"provoke": "login_refused"})
        try:
            ftp(s); raise AssertionError("refused")
        except ftplib.error_perm as e:
            assert str(e).startswith("530")
        s.call("PUT", "/files/servers/edi-acme", {"provoke": "none"})

        # SFTP over the same folders.
        try:
            import paramiko
        except ImportError:
            print("  (paramiko not installed: SFTP skipped)")
            paramiko = None
        if paramiko:
            t = paramiko.Transport(("127.0.0.1", s.SFTP_PORT))
            t.connect(username="edi", password="secret")
            key1 = t.get_remote_server_key().get_base64()
            sf = paramiko.SFTPClient.from_transport(t)
            assert sorted(sf.listdir("/out/ocoren")) == ["OCOREN_0142.txt", "OCOREN_0143.txt"], sf.listdir("/out/ocoren")
            with sf.open("/in/notfis/NOTFIS_1042.txt") as fh:
                assert fh.read() == b"000GLOBEX NOTFIS 1042\r\n"
            with sf.open("/out/ocoren/OCOREN_0144.txt", "wb") as fh:
                fh.write(b"sftp upload " * 100)
            assert sf.stat("/out/ocoren/OCOREN_0144.txt").st_size == 1200
            sf.rename("/out/ocoren/OCOREN_0144.txt", "/out/ocoren/OCOREN_0145.txt")
            for bad in (lambda: sf.open("/missing/x.txt", "wb"), lambda: sf.chdir("/missing")):
                try:
                    bad(); raise AssertionError("no such folder")
                except FileNotFoundError:
                    pass
            s.call("GET", "/files/servers/edi-acme/file?path=/missing/x.txt", expect=404)
            sf.remove("/out/ocoren/OCOREN_0145.txt")
            t.close()
            t = paramiko.Transport(("127.0.0.1", s.SFTP_PORT)); t.connect(username="edi", password="secret")
            assert t.get_remote_server_key().get_base64() == key1, "the host key is kept"
            t.close()
            s.call("PUT", "/files/servers/edi-acme", {"provoke": "host_key_changed"})
            t = paramiko.Transport(("127.0.0.1", s.SFTP_PORT)); t.connect(username="edi", password="secret")
            assert t.get_remote_server_key().get_base64() != key1, "host_key_changed shows another key"
            t.close()
            s.call("PUT", "/files/servers/edi-acme", {"provoke": "permission_denied"})
            t = paramiko.Transport(("127.0.0.1", s.SFTP_PORT)); t.connect(username="edi", password="secret")
            sf = paramiko.SFTPClient.from_transport(t)
            try:
                sf.open("/out/x.txt", "wb"); raise AssertionError("denied")
            except PermissionError:
                pass
            t.close()
            s.call("PUT", "/files/servers/edi-acme", {"provoke": "none"})
            try:
                t = paramiko.Transport(("127.0.0.1", s.SFTP_PORT)); t.connect(username="edi", password="wrong"); raise AssertionError("auth")
            except paramiko.AuthenticationException:
                pass
            print("  SFTP: ok")
        log = s.call("GET", "/files/transfers?server=edi-acme")["transfers"]
        ops = {(x["proto"], x["op"], x["ok"]) for x in log}
        assert ("FTP", "put", True) in ops and ("FTPS", "put", True) in ops and ("FTP", "get", False) in ops, ops
    print("Files e2e: ok")


if __name__ == "__main__":
    main()
