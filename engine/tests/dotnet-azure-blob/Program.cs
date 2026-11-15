// The operations a real app's blob storage layer does, with the same SDK, against Stuntman.
// dotnet run -- "<connection string>" [<ip to connect to, for a named host with no hosts entry>]
using System.Net.Sockets;
using System.Text;
using Azure;
using Azure.Storage.Blobs;
using Azure.Storage.Blobs.Models;
using Azure.Storage.Sas;

var conn = args[0];
var opts = new BlobClientOptions();
var handler = new SocketsHttpHandler();
if (args.Length > 1) {
    handler.ConnectCallback = async (ctx, ct) => {
        var s = new Socket(SocketType.Stream, ProtocolType.Tcp) { NoDelay = true };
        await s.ConnectAsync(args[1], ctx.DnsEndPoint.Port, ct);
        return new NetworkStream(s, true);
    };
}
opts.Transport = new Azure.Core.Pipeline.HttpClientTransport(new HttpClient(handler));
var service = new BlobServiceClient(conn, opts);
var c = service.GetBlobContainerClient("acme-docs");
await c.CreateIfNotExistsAsync(PublicAccessType.None);
await c.CreateIfNotExistsAsync(PublicAccessType.None); // 409 handled by the SDK
var b = c.GetBlobClient("2026/11/nda-acme-corp.pdf");
Check(!(await b.ExistsAsync()).Value, "does not exist yet");
await b.UploadAsync(BinaryData.FromString("%PDF-1.4 unencrypted"), overwrite: true);
Check((await b.ExistsAsync()).Value, "exists after upload");
var meta = new Dictionary<string, string> { ["x_encrypted"] = "true", ["x_enc_alg"] = "AES-256-GCM", ["name"] = Convert.ToBase64String(Encoding.UTF8.GetBytes("NDA Acme.pdf")) };
await b.UploadAsync(new MemoryStream(Encoding.UTF8.GetBytes("ciphertext")), new BlobUploadOptions { Metadata = meta, HttpHeaders = new BlobHttpHeaders { ContentType = "application/pdf", CacheControl = "no-cache" } });
var dl = await b.DownloadStreamingAsync();
Check(dl.Value.Details.Metadata["x_encrypted"] == "true", "metadata comes back on download");
Check(new StreamReader(dl.Value.Content).ReadToEnd() == "ciphertext", "content");
var props = (await b.GetPropertiesAsync()).Value;
Check(props.ContentType == "application/pdf" && props.CacheControl == "no-cache", "http headers kept");
// Conditional upload with the ETag (encryption worker)
await b.UploadAsync(BinaryData.FromString("v2"), new BlobUploadOptions { Conditions = new BlobRequestConditions { IfMatch = props.ETag } });
try {
    await b.UploadAsync(BinaryData.FromString("v3"), new BlobUploadOptions { Conditions = new BlobRequestConditions { IfMatch = props.ETag } });
    Check(false, "stale ETag must fail");
} catch (RequestFailedException e) { Check(e.Status == 412, "412 on a stale ETag"); }
// Copy and wait
var copy = c.GetBlobClient("2026/11/copy.pdf");
var op = await copy.StartCopyFromUriAsync(b.Uri);
await op.WaitForCompletionAsync();
Check((await copy.GetPropertiesAsync()).Value.CopyStatus == CopyStatus.Success, "copy succeeded");
// List with metadata
var names = new List<string>();
await foreach (var item in c.GetBlobsAsync(BlobTraits.Metadata, BlobStates.None, "2026/", default)) names.Add(item.Name);
Check(names.SequenceEqual(new[] { "2026/11/copy.pdf", "2026/11/nda-acme-corp.pdf" }), "list: " + string.Join(",", names));
// SAS signed with the account key, read through plain HTTP
var sas = b.GenerateSasUri(BlobSasPermissions.Read, DateTimeOffset.UtcNow.AddMonths(3));
using var http = new HttpClient(handler, false);
var r = await http.GetAsync(sas);
Check(r.IsSuccessStatusCode && await r.Content.ReadAsStringAsync() == "v2", $"SAS read {(int)r.StatusCode}");
// Delete with snapshots
Check((await copy.DeleteIfExistsAsync(DeleteSnapshotsOption.IncludeSnapshots)).Value, "deleted");
Check(!(await copy.DeleteIfExistsAsync(DeleteSnapshotsOption.IncludeSnapshots)).Value, "404 is fine");
// Several blobs, nested names: each keeps its own name and content (host-style regression), and the
// listing brings their metadata (include=metadata in any case).
var u = service.GetBlobContainerClient("acme-uploads");
await u.CreateIfNotExistsAsync();
var files = new Dictionary<string, string> { ["one.txt"] = "first", ["2026/11/two.txt"] = "second", ["a/b/c/three.txt"] = "third" };
foreach (var (n, body) in files)
    await u.GetBlobClient(n).UploadAsync(BinaryData.FromString(body), new BlobUploadOptions { Metadata = new Dictionary<string, string> { ["origin"] = n.Split('/')[0] } });
var listed = new Dictionary<string, IDictionary<string, string>>();
await foreach (var item in u.GetBlobsAsync(BlobTraits.Metadata, BlobStates.None, null, default)) listed[item.Name] = item.Metadata;
Check(listed.Keys.OrderBy(x => x).SequenceEqual(files.Keys.OrderBy(x => x)), "three blobs listed: " + string.Join(",", listed.Keys));
Check(files.Keys.All(n => listed[n].TryGetValue("origin", out var o) && o == n.Split('/')[0]), "listing carries each blob's metadata");
foreach (var (n, body) in files)
    Check((await u.GetBlobClient(n).DownloadContentAsync()).Value.Content.ToString() == body, "download " + n);
Console.WriteLine("dotnet Azure.Storage.Blobs: all checks passed");

static void Check(bool ok, string what) { if (!ok) { Console.Error.WriteLine("FAILED: " + what); Environment.Exit(1); } Console.WriteLine("ok  " + what); }
