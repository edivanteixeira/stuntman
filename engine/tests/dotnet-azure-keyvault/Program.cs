// What an app does with the Azure SDK, against Stuntman: a managed identity token (App Service
// IDENTITY_ENDPOINT/IDENTITY_HEADER), secrets, a key that signs, a certificate's .pfx.
// IDENTITY_ENDPOINT=http://localhost:8025/metadata/identity/oauth2/token IDENTITY_HEADER=acme-api-mi \
//   dotnet run -- https://acme-kv-stg.vault.stg-a.duble.local:8444 http://localhost:8025/api/test-ca.pem
using System.Net.Security;
using System.Net.Sockets;
using System.Security.Cryptography;
using System.Security.Cryptography.X509Certificates;
using Azure;
using Azure.Core.Pipeline;
using Azure.Identity;
using Azure.Security.KeyVault.Certificates;
using Azure.Security.KeyVault.Keys;
using Azure.Security.KeyVault.Keys.Cryptography;
using Azure.Security.KeyVault.Secrets;

var vault = new Uri(args[0]);
using var boot = new HttpClient();
var ca = X509Certificate2.CreateFromPem(await boot.GetStringAsync(args[1]));
// The vault's host name is not in DNS: connect to localhost, keep the name for TLS (SNI), trust the Test CA.
var handler = new SocketsHttpHandler {
    ConnectCallback = async (ctx, ct) => { var s = new Socket(SocketType.Stream, ProtocolType.Tcp); await s.ConnectAsync("127.0.0.1", ctx.DnsEndPoint.Port, ct); return new NetworkStream(s, true); },
    SslOptions = new SslClientAuthenticationOptions {
        RemoteCertificateValidationCallback = (_, cert, _, _) => {
            using var chain = new X509Chain();
            chain.ChainPolicy.TrustMode = X509ChainTrustMode.CustomRootTrust;
            chain.ChainPolicy.CustomTrustStore.Add(ca);
            chain.ChainPolicy.RevocationMode = X509RevocationMode.NoCheck;
            return cert is not null && chain.Build(new X509Certificate2(cert));
        } },
};
var transport = new HttpClientTransport(new HttpClient(handler));
var cred = new ManagedIdentityCredential();

var secrets = new SecretClient(vault, cred, new SecretClientOptions { Transport = transport });
await secrets.SetSecretAsync("dotnet-check", "synthetic-1");
var s = (await secrets.GetSecretAsync("dotnet-check")).Value;
Check(s.Value == "synthetic-1", "set and get a secret");
var names = new List<string>();
await foreach (var p in secrets.GetPropertiesOfSecretsAsync()) names.Add(p.Name);
Check(names.Contains("dotnet-check"), "list secrets");
try { await secrets.GetSecretAsync("does-not-exist"); Check(false, "404"); }
catch (RequestFailedException e) { Check(e.Status == 404 && e.ErrorCode == "SecretNotFound", "404 SecretNotFound"); }

var keys = new KeyClient(vault, cred, new KeyClientOptions { Transport = transport });
var key = (await keys.CreateRsaKeyAsync(new CreateRsaKeyOptions("dotnet-signing") { KeySize = 2048 })).Value;
var crypto = new CryptographyClient(key.Id, cred, new CryptographyClientOptions { Transport = transport });
var digest = SHA256.HashData("contract.pdf"u8.ToArray());
var sig = (await crypto.SignAsync(SignatureAlgorithm.RS256, digest)).Signature;
Check((await crypto.VerifyAsync(SignatureAlgorithm.RS256, digest, sig)).IsValid, "sign and verify in the vault");
using var rsa = key.Key.ToRSA();
Check(rsa.VerifyHash(digest, sig, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1), "signature verifies with the public key, outside");
var wrapped = (await crypto.WrapKeyAsync(KeyWrapAlgorithm.RsaOaep, new byte[32])).EncryptedKey;
Check((await crypto.UnwrapKeyAsync(KeyWrapAlgorithm.RsaOaep, wrapped)).Key.Length == 32, "wrap and unwrap");

var yearly = (await keys.CreateRsaKeyAsync(new CreateRsaKeyOptions("dotnet-yearly") { KeySize = 2048, ExpiresOn = DateTimeOffset.UtcNow.AddYears(1), KeyOperations = { KeyOperation.Sign, KeyOperation.Verify } })).Value;
Check(yearly.Properties.ExpiresOn > DateTimeOffset.UtcNow.AddDays(360), "a key with an expiry date keeps it");
var certs = new CertificateClient(vault, cred, new CertificateClientOptions { Transport = transport });
var op = await certs.StartCreateCertificateAsync("dotnet-cert", CertificatePolicy.Default);
await op.WaitForCompletionAsync();
using var x = (await certs.DownloadCertificateAsync("dotnet-cert")).Value;
Check(x.HasPrivateKey && x.Subject.Contains("CN="), "download the certificate with its private key (.pfx)");
Console.WriteLine("dotnet Azure.Security.KeyVault: all checks passed");

static void Check(bool ok, string what) { if (!ok) { Console.Error.WriteLine("FAILED: " + what); Environment.Exit(1); } Console.WriteLine("ok  " + what); }
