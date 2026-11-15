// Stuntman's ActiveMQ (OpenWire) broker, checked with the official .NET client the way a real
// app uses it: NMSConnectionFactory, user and password, a session, queue://positions, a producer
// (persistent sends wait for the broker's Response), a consumer and Receive(timeout) until null.
// Usage: dotnet run -- <activemq:tcp://127.0.0.1:61616> <user> <password> <mode> [arg]
//   run          produce 3 text messages, consume until null, then log in with a wrong password
//   consume      consume queue://positions until null (prints what came, redelivered or not); arg
//                "client": ClientAcknowledge mode (message.Acknowledge())
//   login        only connect (prints "login: ok" or the exception)
//   tx           a transacted session on queue://orders.tx: a send rolled back, two sends invisible
//                until the commit, a receive rolled back and redelivered, then committed
//   txfail       one transacted send whose commit the broker refuses (set_activemq fail_commits)
//   durable      a durable subscriber (clientId acme-dashboard, "prices-sub", region = 'north') on
//                topic://prices: consume until null and disconnect; arg "unsubscribe": then remove it
//   select       queue://orders.sel with the selector region = 'north' AND amount > 100, then an
//                invalid selector
//   groups       two consumers on queue://orders.grp, read in turns (run it with prefetch 1)
//   produce      send arg as a text message to queue://orders.z (with nms.UseCompression=true, compressed)
//   drain        consume a queue (arg) until null, printing what came in order
using Apache.NMS;
using Apache.NMS.Util;

var uri = args.Length > 0 ? args[0] : "activemq:tcp://127.0.0.1:61616";
var user = args.Length > 1 ? args[1] : "acme";
var password = args.Length > 2 ? args[2] : "s3cret";
var mode = args.Length > 3 ? args[3] : "run";
var arg = args.Length > 4 ? args[4] : "";
var ack = arg == "client" ? AcknowledgementMode.ClientAcknowledge : AcknowledgementMode.AutoAcknowledge;
int failed = 0;
void Check(bool ok, string what) { Console.WriteLine((ok ? "ok   " : "FAIL ") + what); if (!ok) failed++; }

string Show(IMessage m)
{
    var text = m is ITextMessage t ? t.Text : m.GetType().Name;
    var region = m.Properties.Contains("region") ? m.Properties.GetString("region") : "";
    return $"{text} region={region} redelivered={m.NMSRedelivered} id={m.NMSMessageId}";
}

int Consume(ISession session, IDestination dest, string? selector = null)
{
    using var consumer = selector == null ? session.CreateConsumer(dest) : session.CreateConsumer(dest, selector);
    int n = 0;
    while (true)
    {
        var msg = consumer.Receive(TimeSpan.FromSeconds(3));
        if (msg == null) break;
        n++;
        Console.WriteLine("got: " + Show(msg));
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
    if (mode == "durable") connection.ClientId = "acme-dashboard";
    connection.Start();
    switch (mode)
    {
        case "run":
        case "consume":
        {
            using var session = connection.CreateSession(ack);
            var destination = SessionUtil.GetDestination(session, "queue://positions");
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
            Console.WriteLine($"consumed: {Consume(session, destination)}");
            break;
        }
        case "tx":
        {
            using var txs = connection.CreateSession(AcknowledgementMode.Transactional);
            using var plain = connection.CreateSession(AcknowledgementMode.AutoAcknowledge);
            var q = SessionUtil.GetDestination(txs, "queue://orders.tx");
            using var producer = txs.CreateProducer(q);
            producer.DeliveryMode = MsgDeliveryMode.Persistent;
            producer.Send(txs.CreateTextMessage("ORD-0"));
            txs.Rollback();
            producer.Send(txs.CreateTextMessage("ORD-1"));
            producer.Send(txs.CreateTextMessage("ORD-2"));
            using (var peek = plain.CreateConsumer(q))
                Check(peek.Receive(TimeSpan.FromSeconds(1)) == null, "sends inside an open transaction are not visible");
            txs.Commit();
            Console.WriteLine("committed 2 sends");
            using var consumer = txs.CreateConsumer(q);
            var first = consumer.Receive(TimeSpan.FromSeconds(3)) as ITextMessage;
            Check(first?.Text == "ORD-1" && !first.NMSRedelivered, "transacted receive got ORD-1 (" + first?.Text + ")");
            txs.Rollback();
            var again = consumer.Receive(TimeSpan.FromSeconds(3)) as ITextMessage;
            Check(again?.Text == "ORD-1" && again.NMSRedelivered, "after the rollback ORD-1 came again, redelivered (" + again?.Text + " " + again?.NMSRedelivered + ")");
            var second = consumer.Receive(TimeSpan.FromSeconds(3)) as ITextMessage;
            Check(second?.Text == "ORD-2", "then ORD-2");
            txs.Commit();
            Check(consumer.Receive(TimeSpan.FromSeconds(1)) == null, "nothing left after the commit");
            break;
        }
        case "txfail":
        {
            using var txs = connection.CreateSession(AcknowledgementMode.Transactional);
            using var producer = txs.CreateProducer(SessionUtil.GetDestination(txs, "queue://orders.tx"));
            producer.Send(txs.CreateTextMessage("ORD-9"));
            try { txs.Commit(); Console.WriteLine("commit: ok"); }
            catch (Exception e) { Console.WriteLine($"commit: {e.GetType().Name}: {e.Message}"); }
            break;
        }
        case "durable":
        {
            using var session = connection.CreateSession(AcknowledgementMode.AutoAcknowledge);
            var topic = (ITopic)SessionUtil.GetDestination(session, "topic://prices");
            int n = 0;
            using (var sub = session.CreateDurableConsumer(topic, "prices-sub", "region = 'north'", false))
            {
                Console.WriteLine("subscribed: prices-sub");
                while (sub.Receive(TimeSpan.FromSeconds(3)) is IMessage m) { n++; Console.WriteLine("got: " + Show(m)); }
            }
            Console.WriteLine($"consumed: {n}");
            if (arg == "unsubscribe")
            {
                session.DeleteDurableConsumer("prices-sub");
                Console.WriteLine("unsubscribed");
            }
            break;
        }
        case "select":
        {
            using var session = connection.CreateSession(AcknowledgementMode.AutoAcknowledge);
            var q = SessionUtil.GetDestination(session, "queue://orders.sel");
            Console.WriteLine($"consumed: {Consume(session, q, "region = 'north' AND amount > 100")}");
            try
            {
                using var bad = session.CreateConsumer(q, "region = = 'x'");
                Console.WriteLine("invalid selector: accepted");
            }
            catch (Exception e) { Console.WriteLine($"invalid selector: {e.GetType().Name}: {e.Message}"); }
            break;
        }
        case "groups":
        {
            using var session = connection.CreateSession(AcknowledgementMode.AutoAcknowledge);
            var q = SessionUtil.GetDestination(session, "queue://orders.grp");
            using var c1 = session.CreateConsumer(q);
            using var c2 = session.CreateConsumer(q);
            // Which consumer owned which group is checked through the tools (the consumer of each message).
            int n = 0;
            for (bool more = true; more; )
            {
                more = false;
                foreach (var (name, c) in new[] { ("c1", c1), ("c2", c2) })
                {
                    if (c.Receive(TimeSpan.FromSeconds(2)) is not IMessage m) continue;
                    more = true;
                    n++;
                    Console.WriteLine($"{name} got: {Show(m)}");
                }
            }
            Console.WriteLine($"consumed: {n}");
            break;
        }
        case "produce":
        {
            using var session = connection.CreateSession(AcknowledgementMode.AutoAcknowledge);
            using var producer = session.CreateProducer(SessionUtil.GetDestination(session, "queue://orders.z"));
            producer.DeliveryMode = MsgDeliveryMode.Persistent;
            producer.Send(session.CreateTextMessage(arg));
            Console.WriteLine("produced");
            break;
        }
        case "drain":
        {
            using var session = connection.CreateSession(AcknowledgementMode.AutoAcknowledge);
            Console.WriteLine($"consumed: {Consume(session, SessionUtil.GetDestination(session, arg))}");
            break;
        }
    }
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
