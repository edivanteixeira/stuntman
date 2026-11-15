import java.net.URI;
import java.time.Duration;
import java.util.List;

import software.amazon.awssdk.auth.credentials.AwsBasicCredentials;
import software.amazon.awssdk.auth.credentials.StaticCredentialsProvider;
import software.amazon.awssdk.core.SdkBytes;
import software.amazon.awssdk.core.retry.backoff.FixedDelayBackoffStrategy;
import software.amazon.awssdk.core.waiters.WaiterOverrideConfiguration;
import software.amazon.awssdk.regions.Region;
import software.amazon.awssdk.services.kinesis.KinesisClient;
import software.amazon.awssdk.services.kinesis.model.*;

/** Kinesis through the AWS SDK for Java v2 (CBOR): create, wait for ACTIVE, put, describe, read, PutRecords. */
public class Main {
    static void check(boolean ok, String what) {
        if (!ok) { System.err.println("FAIL " + what); System.exit(1); }
    }

    public static void main(String[] a) {
        KinesisClient k = KinesisClient.builder().endpointOverride(URI.create(a[0])).region(Region.US_EAST_1)
            .credentialsProvider(StaticCredentialsProvider.create(AwsBasicCredentials.create("AKIAEXAMPLE", "any"))).build();
        String name = "java-shipments";
        k.createStream(b -> b.streamName(name).shardCount(1));
        check(k.describeStreamSummary(b -> b.streamName(name)).streamDescriptionSummary().streamStatus() == StreamStatus.CREATING, "CREATING after CreateStream");
        k.waiter().waitUntilStreamExists(DescribeStreamRequest.builder().streamName(name).build(),
            WaiterOverrideConfiguration.builder().backoffStrategy(FixedDelayBackoffStrategy.create(Duration.ofMillis(500))).maxAttempts(20).build());
        PutRecordResponse p = k.putRecord(b -> b.streamName(name).partitionKey("SHP-7001").data(SdkBytes.fromUtf8String("{\"shipment\": \"SHP-7001\", \"status\": \"CREATED\"}")));
        System.out.println("put " + p.shardId() + " " + p.sequenceNumber());
        StreamDescription d = k.describeStream(b -> b.streamName(name)).streamDescription();
        check(d.streamStatus() == StreamStatus.ACTIVE && d.shards().size() == 1 && d.streamARN().endsWith(":stream/" + name), "describe " + d);
        PutRecordsResponse pr = k.putRecords(b -> b.streamName(name).records(List.of(
            PutRecordsRequestEntry.builder().partitionKey("SHP-7002").data(SdkBytes.fromUtf8String("{\"shipment\": \"SHP-7002\"}")).build(),
            PutRecordsRequestEntry.builder().partitionKey("SHP-7003").data(SdkBytes.fromByteArray(new byte[]{0, 1, 2})).build())));
        check(pr.failedRecordCount() == 0 && pr.records().size() == 2, "PutRecords " + pr);
        String it = k.getShardIterator(b -> b.streamName(name).shardId(p.shardId()).shardIteratorType(ShardIteratorType.TRIM_HORIZON)).shardIterator();
        GetRecordsResponse r = k.getRecords(b -> b.shardIterator(it));
        check(r.records().size() == 3, "3 records, got " + r.records().size());
        check(r.records().get(0).data().asUtf8String().contains("SHP-7001") && r.records().get(0).approximateArrivalTimestamp() != null, "first record " + r.records().get(0));
        check(r.records().get(2).data().asByteArray().length == 3, "binary record");
        try {
            k.describeStream(b -> b.streamName("java-missing"));
            check(false, "ResourceNotFoundException");
        } catch (ResourceNotFoundException e) {
            System.out.println("not found: " + e.awsErrorDetails().errorCode());
        }
        System.out.println("java ok " + r.records().size() + " records, millisBehindLatest " + r.millisBehindLatest());
    }
}
