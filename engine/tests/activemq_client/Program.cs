// Stuntman's ActiveMQ (OpenWire) broker, checked with the official .NET client the way a real
// app uses it: NMSConnectionFactory, user and password, a session, queue://positions, a producer
// (persistent sends wait for the broker's Response), a consumer and Receive(timeout) until null.
// Usage: dotnet run -- <activemq:tcp://127.0.0.1:61616> <user> <password> <mode> [client]
//   run      produce 3 text messages, consume until null, then log in with a wrong password
//   consume  consume until null (prints what came, redelivered or not)
//   login    only connect (prints "login: ok" or the exception)
// "client" as 5th argument consumes in ClientAcknowledge mode (message.Acknowledge()).
using Apache.NMS;
using Apache.NMS.Util;

var uri = args.Length > 0 ? args[0] : "activemq:tcp://127.0.0.1:61616";
var user = args.Length > 1 ? args[1] : "acme";
var password = args.Length > 2 ? args[2] : "s3cret";
var mode = args.Length > 3 ? args[3] : "run";
var ack = args.Length > 4 && args[4] == "client" ? AcknowledgementMode.ClientAcknowledge : AcknowledgementMode.AutoAcknowledge;
int failed = 0;
void Check(bool ok, string what) { Console.WriteLine((ok ? "ok   " : "FAIL ") + what); if (!ok) failed++; }

int Consume(ISession session, IDestination dest)
{
    using var consumer = session.CreateConsumer(dest);
    int n = 0;
    while (true)
    {
        var msg = consumer.Receive(TimeSpan.FromSeconds(3)) as ITextMessage;
        if (msg == null) break;
        n++;
        var region = msg.Properties.Contains("region") ? msg.Properties.GetString("region") : "";
        Console.WriteLine($"got: {msg.Text} region={region} redelivered={msg.NMSRedelivered} id={msg.NMSMessageId}");
        if (ack == AcknowledgementMode.ClientAcknowledge) msg.Acknowledge();
    }
    return n;
}

string Login(string u, string p)
{
    try
    {
        var f = new NMSConnectionFactory(new Uri(uri));
        using var c = f.CreateConnection(u, p);
        c.Start();
        using var s = c.CreateSession();
        return "ok";
    }
    catch (NMSSecurityException e) { return "security: " + e.Message; }
    catch (Exception e) { return e.GetType().Name + ": " + e.Message; }
}

if (mode == "login")
{
    Console.WriteLine("login: " + Login(user, password));
    return 0;
}

var factory = new NMSConnectionFactory(new Uri(uri));
try
{
    using var connection = factory.CreateConnection(user, password);
    using var session = connection.CreateSession(ack);
    var destination = SessionUtil.GetDestination(session, "queue://positions");
    connection.Start();
    if (mode == "run")
    {
        using (var producer = session.CreateProducer(destination))
        {
            producer.DeliveryMode = MsgDeliveryMode.Persistent;
            foreach (var truck in new[] { "TRK-100", "TRK-200", "TRK-300" })
            {
                var m = session.CreateTextMessage($"{{\"truck\":\"{truck}\",\"lat\":-23.5,\"lon\":-46.6}}");
                m.Properties.SetString("region", "north");
                m.NMSCorrelationID = "acme-" + truck;
                producer.Send(m);
            }
        }
        Check(true, "produced 3 text messages to queue://positions");
    }
    var got = Consume(session, destination);
    Console.WriteLine($"consumed: {got}");
}
catch (Exception e)
{
    Console.WriteLine($"error: {e.GetType().Name}: {e.Message}");
    failed++;
}

if (mode == "run")
{
    var bad = Login(user, password + "-wrong");
    Check(bad.StartsWith("security:"), "wrong password refused with NMSSecurityException (" + bad + ")");
}
return failed;
