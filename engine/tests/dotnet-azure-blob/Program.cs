// The operations a real app's blob storage layer does, with the same SDK, against Stuntman.
// dotnet run -- "<connection string>"
using System.Text;
using Azure;
using Azure.Storage.Blobs;
using Azure.Storage.Blobs.Models;
using Azure.Storage.Sas;

var conn = args[0];
var service = new BlobServiceClient(conn);
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
using var http = new HttpClient();
var r = await http.GetAsync(sas);
Check(r.IsSuccessStatusCode && await r.Content.ReadAsStringAsync() == "v2", $"SAS read {(int)r.StatusCode}");
// Delete with snapshots
Check((await copy.DeleteIfExistsAsync(DeleteSnapshotsOption.IncludeSnapshots)).Value, "deleted");
Check(!(await copy.DeleteIfExistsAsync(DeleteSnapshotsOption.IncludeSnapshots)).Value, "404 is fine");
Console.WriteLine("dotnet Azure.Storage.Blobs: all checks passed");

static void Check(bool ok, string what) { if (!ok) { Console.Error.WriteLine("FAILED: " + what); Environment.Exit(1); } Console.WriteLine("ok  " + what); }
