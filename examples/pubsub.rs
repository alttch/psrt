use psrt::DEFAULT_PRIORITY;
use psrt::client::{Client, Config};
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() {
    let test_topic = "test/topic1";
    // define client configuration
    let config = Config::new("localhost:2873")
        .set_timeout(Duration::from_secs(5))
        .build();
    // connect PSRT client
    let mut client = Client::connect(&config).await.expect("Failed to connect");
    // subscribe to the topic
    client.subscribe(test_topic.to_owned()).await.unwrap();
    // get data channel
    let data_channel = client.take_data_channel().unwrap();
    let receiver_fut = tokio::spawn(async move {
        // receive messages from the server
        while let Ok(message) = data_channel.recv().await {
            println!(
                "topic: {}, data: {}",
                message.topic(),
                message.data_as_str().unwrap()
            );
        }
    });
    for _ in 0..3 {
        // if required, check that the client is still connected
        assert!(client.is_connected());
        // publish a message
        client
            .publish(
                DEFAULT_PRIORITY,
                test_topic.to_owned(),
                "hello".as_bytes().to_vec(),
            )
            .await
            .unwrap();
        sleep(Duration::from_secs(1)).await;
    }
    client.bye().await.unwrap();
    receiver_fut.await.unwrap();
}
