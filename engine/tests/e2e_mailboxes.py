"""End-to-end check of mailboxes (A16): a message with an XML attachment put in a mailbox, read by
the app over IMAP (STARTTLS, SEARCH, BODYSTRUCTURE, BODY.PEEK[], STORE, EXPUNGE, CREATE, MOVE, IDLE)
and POP3 (STLS, STAT, UIDL, RETR, DELE); the failures provoked: login refused, mailbox in use,
connection dropped during FETCH, TLS required, XOAUTH2 token expired. With Python's imaplib and poplib.

    cargo build -p stuntman && python3 tests/e2e_mailboxes.py
"""
import base64, email, imaplib, poplib, re, socket, ssl, time

from harness import Stuntman

XML = '<nfeProc xmlns="http://www.portalfiscal.inf.br/nfe"><NFe><infNFe Id="NFe35261112345678000199550010000012341000012345"/></NFe></nfeProc>'


def tls_ctx():
    c = ssl.create_default_context()
    c.check_hostname = False
    c.verify_mode = ssl.CERT_NONE
    return c


def imap(s, user="nfe@acme.example", pw="s3cret", tls=True):
    m = imaplib.IMAP4("127.0.0.1", s.IMAP_PORT)
    if tls:
        m.starttls(tls_ctx())
    m.login(user, pw)
    return m


def main():
    with Stuntman("stuntman-mail-") as s:
        s.call("PUT", "/mailboxes/nfe@acme.example", {"password": "s3cret"})
        r = s.call("POST", "/mailboxes/nfe@acme.example/messages", {"from": "Fornecedor Globex <fornecedor@globex.example>", "subject": "NF-e 1234",
            "text": "Segue a NF-e.", "attachments": [{"filename": "35261112345678000199550010000012341000012345-procNFe.xml", "content_type": "application/xml", "text": XML}]})
        assert r["uid"] == 1, r
        s.call("POST", "/mailboxes/nfe@acme.example/messages", {"from": "outro@globex.example", "subject": "Boleto", "text": "sem anexo"})

        m = imap(s)
        typ, d = m.select("INBOX")
        assert d == [b"2"], d
        typ, d = m.search(None, "UNSEEN")
        assert d == [b"1 2"], d
        typ, d = m.uid("SEARCH", None, "SUBJECT", '"NF-e"')
        assert d == [b"1"], d
        typ, d = m.fetch("1", "(UID FLAGS RFC822.SIZE ENVELOPE BODYSTRUCTURE)")
        meta = d[0].decode() if isinstance(d[0], bytes) else d[0][0].decode()
        assert "UID 1" in meta and '"application" "xml"' in meta and '"NF-e 1234"' in meta and '"fornecedor" "globex.example"' in meta, meta
        typ, d = m.fetch("1", "(BODY.PEEK[])")
        msg = email.message_from_bytes(d[0][1])
        att = [p for p in msg.walk() if p.get_filename()]
        assert att[0].get_payload(decode=True).decode() == XML, att
        typ, d = m.fetch("1", "(FLAGS)")
        assert b"\\Seen" not in d[0], d
        typ, d = m.fetch("1", "(BODY[2])")
        assert base64.b64decode(d[0][1]).decode() == XML
        typ, d = m.fetch("1", "(FLAGS)")
        assert b"\\Seen" in d[0], d
        typ, d = m.fetch("2", "(BODY.PEEK[HEADER.FIELDS (SUBJECT)])")
        assert d[0][1].strip() == b"Subject: Boleto", d
        m.create("Processadas")
        typ, d = m.uid("MOVE", "1", "Processadas")
        assert typ == "OK", (typ, d)
        typ, d = m.select("Processadas")
        assert d == [b"1"], d
        m.select("INBOX")
        m.store("1", "+FLAGS", "\\Deleted")
        typ, d = m.expunge()
        assert d == [b"1"], d
        typ, d = m.status("INBOX", "(MESSAGES UNSEEN)")
        assert b"MESSAGES 0" in d[0], d
        # IDLE: a new message shows up as EXISTS.
        tag = m._new_tag().decode()
        m.send(f"{tag} IDLE\r\n".encode())
        assert m.readline().startswith(b"+")
        s.call("POST", "/mailboxes/nfe@acme.example/messages", {"subject": "NF-e 1235", "text": "nova"})
        line = m.readline()
        assert re.match(rb"\* 1 EXISTS", line), line
        m.send(b"DONE\r\n")
        assert m.readline().startswith(tag.encode() + b" OK"), "IDLE ends"
        m.logout()

        # POP3.
        p = poplib.POP3("127.0.0.1", s.POP3_PORT)
        p.stls(tls_ctx())
        p.user("nfe@acme.example"); p.pass_("s3cret")
        n, size = p.stat()
        assert n == 1, (n, size)
        assert p.uidl()[1][0].split()[0] == b"1"
        lines = p.retr(1)[1]
        assert b"Subject: NF-e 1235" in b"\n".join(lines)
        p.dele(1)
        p.quit()
        assert s.call("GET", "/mailboxes/nfe@acme.example/messages")["messages"] == []

        # Provocations.
        s.call("POST", "/mailboxes/nfe@acme.example/messages", {"subject": "big", "text": "x" * 5000})
        s.call("PUT", "/mailboxes/nfe@acme.example", {"provoke": "drop_fetch"})
        m = imap(s)
        m.select("INBOX")
        try:
            m.fetch("1", "(BODY.PEEK[])")
            raise AssertionError("drop")
        except (imaplib.IMAP4.abort, OSError, imaplib.IMAP4.error):
            pass
        s.call("PUT", "/mailboxes/nfe@acme.example", {"provoke": "busy"})
        m = imap(s)
        typ, d = m.select("INBOX")
        assert typ == "NO" and b"INUSE" in d[0], (typ, d)
        m.logout()
        s.call("PUT", "/mailboxes/nfe@acme.example", {"provoke": "tls_required"})
        try:
            imap(s, tls=False); raise AssertionError("tls required")
        except imaplib.IMAP4.error as e:
            assert "PRIVACYREQUIRED" in str(e), e
        imap(s).logout()
        s.call("PUT", "/mailboxes/nfe@acme.example", {"provoke": "token_expired"})
        m = imaplib.IMAP4("127.0.0.1", s.IMAP_PORT)
        try:
            m.authenticate("XOAUTH2", lambda _: b"user=nfe@acme.example\x01auth=Bearer ya29.expired\x01\x01")
            raise AssertionError("token")
        except imaplib.IMAP4.error:
            pass
        s.call("PUT", "/mailboxes/nfe@acme.example", {"provoke": "login_refused"})
        try:
            imap(s); raise AssertionError("refused")
        except imaplib.IMAP4.error as e:
            assert "AUTHENTICATIONFAILED" in str(e), e
        s.call("PUT", "/mailboxes/nfe@acme.example", {"provoke": "none"})
        m = imaplib.IMAP4("127.0.0.1", s.IMAP_PORT)
        typ, _ = m.authenticate("XOAUTH2", lambda _: b"user=nfe@acme.example\x01auth=Bearer ya29.ok\x01\x01")
        assert typ == "OK"
        m.logout()
        log = s.call("GET", "/mailboxes/nfe@acme.example/messages")["log"]
        assert not any("s3cret" in x["line"] for x in log), "no passwords in the log"
        assert any("FETCH" in x["line"] for x in log), log[:5]
    print("Mailboxes e2e: ok")


if __name__ == "__main__":
    main()
