#[tokio::main(flavor = "current_thread")]
async fn main() {
    // plain
    let client = psrt::client::UdpClient::connect("127.0.0.1:2873")
        .await
        .unwrap()
        .with_auth("user1", "xxx");
    client.publish("mytopic", "hello".as_bytes()).await.unwrap();
    // secure
    let key =
        hex::decode("26fd38045707792a9bc50f3761a58987c4a9362cf60389f341c28e37b1125d93").unwrap();
    let client = psrt::client::UdpClient::connect("127.0.0.1:2873")
        .await
        .unwrap()
        .with_encryption_auth("user1", &key);
    client
        .publish("mytopic", "hello secure".as_bytes())
        .await
        .unwrap();
}
