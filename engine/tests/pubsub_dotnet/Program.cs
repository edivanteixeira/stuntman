// Stuntman's Pub/Sub side, checked with the official .NET client the way an app publishes: a
// PublisherClient built with EmulatorDetection.EmulatorOrProduction (PUBSUB_EMULATOR_HOST set),
// PublishAsync(text) three times, then a clean shutdown. Prints one message id per line.
// Usage: PUBSUB_EMULATOR_HOST=localhost:8085 dotnet run -- acme-stg shipments
using Google.Api.Gax;
using Google.Cloud.PubSub.V1;

var project = args.Length > 0 ? args[0] : "acme-stg";
var topic = args.Length > 1 ? args[1] : "shipments";
var publisher = await new PublisherClientBuilder
{
    TopicName = TopicName.FromProjectTopic(project, topic),
    EmulatorDetection = EmulatorDetection.EmulatorOrProduction,
}.BuildAsync();
foreach (var text in new[] { "SHP-2001 picked up", "SHP-2001 in transit", "SHP-2001 delivered" })
{
    var id = await publisher.PublishAsync(text);
    Console.WriteLine($"id {id}");
}
await publisher.ShutdownAsync(TimeSpan.FromSeconds(10));
