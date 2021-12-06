use aes_gcm::aead::{Aead, NewAead};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use std::net::UdpSocket;

fn main() {
    let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    let aes_key =
        hex::decode("26fd38045707792a9bc50f3761a58987c4a9362cf60389f341c28e37b1125d93").unwrap();
    let aes_nonce = hex::decode("5c088bee9dea47a5ee31c2eb").unwrap();
    let key = Key::from_slice(&aes_key);
    let cipher = Aes256Gcm::new(key);
    let nonce = Nonce::from_slice(&aes_nonce);
    let mut packet: Vec<u8> = Vec::new();
    packet.extend_from_slice(&psrt::CONTROL_HEADER);
    packet.extend_from_slice(&psrt::PROTOCOL_VERSION.to_le_bytes());
    packet.push(psrt::AUTH_KEY_AES256_GCM);
    packet.extend_from_slice("user1".as_bytes());
    packet.push(0x0);
    let mut data: Vec<u8> = vec![psrt::OP_PUBLISH_NO_ACK, psrt::DEFAULT_PRIORITY];
    data.extend_from_slice("mytopic".as_bytes());
    data.push(0x00);
    data.extend_from_slice("hello".as_bytes());
    let enc_block = cipher
        .encrypt(nonce, data.as_ref())
        .expect("encryption failure!");
    packet.extend(enc_block);
    socket.send_to(&packet, "127.0.0.1:2873").unwrap();
}
