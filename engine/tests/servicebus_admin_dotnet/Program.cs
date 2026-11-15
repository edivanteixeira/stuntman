// ServiceBusAdministrationClient against Stuntman's Service Bus management port (5300, as the
// official emulator), with the emulator connection string an app uses: the SDK calls plain HTTP
// on the port the string names. The same string then sends and receives over AMQP on that port.
// Usage: dotnet run -- "Endpoint=sb://sb-admin.localhost:5300;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=any=;UseDevelopmentEmulator=true"
using Azure.Messaging.ServiceBus;
using Azure.Messaging.ServiceBus.Administration;

var cs = args.Length > 0 ? args[0] : "Endpoint=sb://sb-admin.localhost:5300;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=any=;UseDevelopmentEmulator=true";
int failed = 0;
void Check(bool ok, string what) { Console.WriteLine((ok ? "ok   " : "FAIL ") + what); if (!ok) failed++; }

var admin = new ServiceBusAdministrationClient(cs);
const string queue = "invoice-render", topic = "customer-signed-up", sub = "welcome-email";
if (await admin.QueueExistsAsync(queue)) await admin.DeleteQueueAsync(queue);
if (await admin.TopicExistsAsync(topic)) await admin.DeleteTopicAsync(topic);
await admin.CreateQueueAsync(new CreateQueueOptions(queue) { MaxDeliveryCount = 5 });
Check(await admin.QueueExistsAsync(queue), "admin: CreateQueueAsync, then QueueExistsAsync");
await admin.CreateTopicAsync(topic);
await admin.CreateSubscriptionAsync(topic, sub);
Check(await admin.SubscriptionExistsAsync(topic, sub), "admin: CreateTopicAsync + CreateSubscriptionAsync");

await using var client = new ServiceBusClient(cs);
await (client.CreateSender(queue)).SendMessageAsync(new ServiceBusMessage("{\"invoiceId\":\"INV-1042\"}") { Subject = "Render" });
await (client.CreateSender(topic)).SendMessageAsync(new ServiceBusMessage("{\"email\":\"jordan.miller@example.com\"}") { Subject = "SignedUp" });
var q = await client.CreateReceiver(queue).ReceiveMessageAsync(TimeSpan.FromSeconds(10));
Check(q?.Subject == "Render", "queue: send and receive on the same port");
var s = await client.CreateReceiver(topic, sub).ReceiveMessageAsync(TimeSpan.FromSeconds(10));
Check(s?.Subject == "SignedUp", "topic: the subscription got the message");

Console.WriteLine(failed == 0 ? "dotnet servicebus admin: all checks passed" : $"dotnet servicebus admin: {failed} failed");
return failed == 0 ? 0 : 1;
