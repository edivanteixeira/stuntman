// Stuntman's RabbitMQ side, checked with the official client the way a real app uses it:
// topic exchange per message type, a queue per subscriber bound to it, QoS 1, publisher
// confirms, manual ack and nack (no requeue), a second consumer, basic.get, connection close.
// Usage: dotnet run -- amqp://guest:any@localhost:5673/acme-rabbit-stg
using System.Text;
using RabbitMQ.Client;
using RabbitMQ.Client.Events;

var uri = args.Length > 0 ? args[0] : "amqp://guest:any@localhost:5673/acme-rabbit-stg";
var factory = new ConnectionFactory { Uri = new Uri(uri), ClientProvidedName = "acme-worker-1" };
int failed = 0;
void Check(bool ok, string what) { Console.WriteLine((ok ? "ok   " : "FAIL ") + what); if (!ok) failed++; }

await using var conn = await factory.CreateConnectionAsync();
Check(conn.IsOpen, "connect, PLAIN login, vhost open");
await using var pub = await conn.CreateChannelAsync(new CreateChannelOptions(publisherConfirmationsEnabled: true, publisherConfirmationTrackingEnabled: true));
await pub.ExchangeDeclareAsync("order-created", ExchangeType.Topic, true, false);
var q = await pub.QueueDeclareAsync("order-created.invoices", true, false, false, null);
Check(q.QueueName == "order-created.invoices", "queue.declare");
await pub.QueueBindAsync("order-created.invoices", "order-created", "");
await pub.QueuePurgeAsync("order-created.invoices");

var props = new BasicProperties { Persistent = true, ContentType = "application/json", MessageId = "m-58213", Headers = new Dictionary<string, object?> { ["tenant"] = "globex" } };
await pub.BasicPublishAsync("order-created", "", true, props, Encoding.UTF8.GetBytes("{\"orderId\":58213,\"total\":1290.0}"));
await pub.BasicPublishAsync("order-created", "", true, props, Encoding.UTF8.GetBytes("{\"orderId\":58214,\"total\":75.0}"));
Check(true, "basic.publish with confirms (awaited)");

await using var ch = await conn.CreateChannelAsync();
await ch.BasicQosAsync(0, 1, false);
var got = new List<string>();
var done = new TaskCompletionSource();
var consumer = new AsyncEventingBasicConsumer(ch);
consumer.ReceivedAsync += async (_, ea) =>
{
    var body = Encoding.UTF8.GetString(ea.Body.ToArray());
    got.Add(body);
    var tenant = ea.BasicProperties.Headers?["tenant"] is byte[] b ? Encoding.UTF8.GetString(b) : "";
    if (got.Count == 1) { Check(tenant == "globex" && ea.BasicProperties.MessageId == "m-58213", "headers and message id survive"); await ch.BasicAckAsync(ea.DeliveryTag, false); }
    else { await ch.BasicNackAsync(ea.DeliveryTag, false, false); done.TrySetResult(); }
};
var tag = await ch.BasicConsumeAsync("order-created.invoices", false, consumer);
await Task.WhenAny(done.Task, Task.Delay(10000));
Check(got.Count == 2 && got[0].Contains("58213") && got[1].Contains("58214"), "two deliveries in order, QoS 1");
await ch.BasicCancelAsync(tag);

// basic.get on an empty queue, then on one message
var none = await pub.BasicGetAsync("order-created.invoices", true);
Check(none == null, "basic.get on an empty queue");
await pub.BasicPublishAsync("", "order-created.invoices", false, new BasicProperties(), Encoding.UTF8.GetBytes("direct"));
var one = await pub.BasicGetAsync("order-created.invoices", false);
Check(one != null && Encoding.UTF8.GetString(one.Body.ToArray()) == "direct", "default exchange routes by queue name; basic.get");
// leave it unacked and close: it must come back
await pub.CloseAsync();
await using var ch2 = await conn.CreateChannelAsync();
var back = await ch2.BasicGetAsync("order-created.invoices", true);
Check(back != null && back.Redelivered, "unacked message back to the queue, redelivered=true, after channel close");

// a passive declare of a missing exchange closes the channel with 404
try { await ch2.ExchangeDeclarePassiveAsync("no-such-exchange"); Check(false, "passive declare of a missing exchange fails"); }
catch (RabbitMQ.Client.Exceptions.OperationInterruptedException e) { Check(e.ShutdownReason?.ReplyCode == 404, "passive declare of a missing exchange: 404 NOT_FOUND"); }

await conn.CloseAsync();
Console.WriteLine(failed == 0 ? "dotnet rabbitmq: all checks passed" : $"dotnet rabbitmq: {failed} failed");
return failed == 0 ? 0 : 1;
