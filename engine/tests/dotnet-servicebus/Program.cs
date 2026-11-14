// Stuntman's Service Bus side, checked with the official .NET SDK the way a real app uses it:
// the administration client (HTTP, emulator mode) creates a topic and a subscription with
// MaxDeliveryCount 3 and a 2-minute lock; a sender sends now and scheduled; a processor in
// PeekLock with auto lock renewal completes by hand, and a handler that keeps failing ends in
// dead-letter after 3 deliveries.
// Usage: dotnet run -- "Endpoint=sb://sb-dotnet.localhost:5673;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=any=;UseDevelopmentEmulator=true"
using Azure.Messaging.ServiceBus;
using Azure.Messaging.ServiceBus.Administration;

var cs = args.Length > 0 ? args[0] : "Endpoint=sb://sb-dotnet.localhost:5673;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=any=;UseDevelopmentEmulator=true";
int failed = 0;
void Check(bool ok, string what) { Console.WriteLine((ok ? "ok   " : "FAIL ") + what); if (!ok) failed++; }

var admin = new ServiceBusAdministrationClient(cs);
const string topic = "order-shipped-event", sub = "notify-customer";
if (await admin.TopicExistsAsync(topic)) await admin.DeleteTopicAsync(topic);
Check(!await admin.TopicExistsAsync(topic), "admin: TopicExistsAsync on a missing topic is false");
await admin.CreateTopicAsync(new CreateTopicOptions(topic) { SupportOrdering = true });
Check(await admin.TopicExistsAsync(topic), "admin: CreateTopicAsync, then TopicExistsAsync");
await admin.CreateSubscriptionAsync(new CreateSubscriptionOptions(topic, sub) { MaxDeliveryCount = 3, LockDuration = TimeSpan.FromMinutes(2), DefaultMessageTimeToLive = TimeSpan.FromDays(7), DeadLetteringOnMessageExpiration = true });
var props = (await admin.GetSubscriptionAsync(topic, sub)).Value;
Check(props.MaxDeliveryCount == 3 && props.LockDuration == TimeSpan.FromMinutes(2), $"admin: subscription with MaxDeliveryCount {props.MaxDeliveryCount}, lock {props.LockDuration}");

await using var client = new ServiceBusClient(cs);
await using var sender = client.CreateSender(topic);
await sender.SendMessageAsync(new ServiceBusMessage("{\"orderId\":58213}") { Subject = "OrderShipped", ApplicationProperties = { ["tenant"] = "globex" } });
await sender.SendMessageAsync(new ServiceBusMessage("{\"orderId\":666}") { Subject = "Poison" });
await sender.SendMessageAsync(new ServiceBusMessage("{\"orderId\":58214}") { Subject = "Later", ScheduledEnqueueTime = DateTimeOffset.UtcNow.AddSeconds(3) });
Check(true, "send now and scheduled (ScheduledEnqueueTime)");

var completed = new List<string>();
var poisonTries = 0;
var done = new TaskCompletionSource();
await using var processor = client.CreateProcessor(topic, sub, new ServiceBusProcessorOptions { ReceiveMode = ServiceBusReceiveMode.PeekLock, AutoCompleteMessages = false, MaxConcurrentCalls = 1, MaxAutoLockRenewalDuration = TimeSpan.FromMinutes(5) });
processor.ProcessMessageAsync += async a =>
{
    if (a.Message.Subject == "Poison") { poisonTries++; throw new TimeoutException("handler threw TimeoutException"); }
    if (a.Message.Subject == "OrderShipped") Check(a.Message.ApplicationProperties["tenant"]?.ToString() == "globex" && a.Message.LockToken != Guid.Empty.ToString(), "processor: application properties and a lock token");
    await a.CompleteMessageAsync(a.Message);
    lock (completed) completed.Add(a.Message.Subject);
    if (completed.Count == 2 && poisonTries >= 3) done.TrySetResult();
};
processor.ProcessErrorAsync += a => { if (a.Exception is not TimeoutException) Console.WriteLine("  error: " + a.Exception.Message.Split('\n')[0]); return Task.CompletedTask; };
await processor.StartProcessingAsync();
await Task.WhenAny(done.Task, Task.Delay(30000));
await processor.StopProcessingAsync();
Check(completed.Contains("OrderShipped") && completed.Contains("Later"), $"processor completed the message and the scheduled one ({string.Join(", ", completed)})");
Check(poisonTries == 3, $"the failing message was delivered {poisonTries} times (MaxDeliveryCount 3)");

await using var dlq = client.CreateReceiver(topic, sub, new ServiceBusReceiverOptions { SubQueue = SubQueue.DeadLetter });
var dead = await dlq.PeekMessagesAsync(5);
Check(dead.Count == 1 && dead[0].Subject == "Poison", "the failing message is in the dead-letter queue");

Console.WriteLine(failed == 0 ? "dotnet servicebus: all checks passed" : $"dotnet servicebus: {failed} failed");
return failed == 0 ? 0 : 1;
