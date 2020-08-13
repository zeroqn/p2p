use lazy_static::lazy_static;
use prometheus::{register_int_gauge, IntGauge};

lazy_static! {
    pub static ref TENTACLE_MESSAGE_IN_TX_QUEUE: IntGauge = register_int_gauge!(
        "tentacle_message_in_tx_queue",
        "Total number of message in tx queue"
    )
    .expect("tentacle message in tx queue");
    pub static ref TENTACLE_MESSAGE_IN_RX_QUEUE: IntGauge = register_int_gauge!(
        "tentacle_message_in_rx_queue",
        "Total number of message in rx queue"
    )
    .expect("tentacle message in rx queue");
}
