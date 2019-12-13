use std::time::Duration;
use std::error;
use std::net::{SocketAddr};

use tokio::net::TcpStream;
use tokio::io::{BufReader, BufWriter};
use tokio::time::delay_for;
use tokio::task;

use log::{error, debug};

use serde::{Serialize, Deserialize};

use rand::{self, Rng};

use crate::nc_error::{NC_Error};
use crate::nc_server::{NC_ServerMessage};
use crate::nc_util::{nc_send_message, nc_receive_message, nc_encode_data, nc_decode_data, NC_JobStatus};
use crate::nc_config::{NC_Configuration};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NC_NodeMessage {
    NeedsData(u128),
    HasData((u128, Vec<u8>)),
    // NodeHeartBeat(u128),
}

pub trait NC_Node {
    fn process_data_from_server(&mut self, data: Vec<u8>) -> Result<Vec<u8>, Box<dyn error::Error + Send>>;
}

pub async fn start_node<T: NC_Node>(mut nc_node: T, config: NC_Configuration) -> Result<(), NC_Error> {
    let addr = SocketAddr::new(config.address.parse().map_err(|e| NC_Error::IPAddr(e))?, config.port);

    let mut bytes = [0u8; 16];
    rand::thread_rng().fill(&mut bytes[..]);
    let node_id: u128 = u128::from_le_bytes(bytes);

    debug!("Current node id: {}", node_id);

    loop {
        match node_worker(&mut nc_node, &addr, node_id).await {
            Ok(NC_JobStatus::Unfinished) => {
                debug!("Job is not finished yet, back to work!");
            }
            Ok(NC_JobStatus::Waiting) => {
                debug!("Retry in {} seconds", config.reconnect_wait);
                delay_for(Duration::from_secs(config.reconnect_wait)).await;
            }
            Ok(NC_JobStatus::Finished) => {
                debug!("Job is finished, exit loop");
                break
            }
            Err(e) => {
                error!("An error occurred: {}", e);

                debug!("Retry in {} seconds", config.reconnect_wait);
                delay_for(Duration::from_secs(config.reconnect_wait)).await;
            }
        }
    }

    Ok(())
}

pub async fn node_worker<T: NC_Node>(nc_node: &mut T, addr: &SocketAddr, node_id: u128) -> Result<NC_JobStatus, NC_Error> {
    debug!("Connecting to server: {}", addr);
    let mut stream = TcpStream::connect(&addr).await.map_err(|e| NC_Error::TcpConnect(e))?;
    let (reader, writer) = stream.split();
    let mut buf_reader = BufReader::new(reader);
    let mut buf_writer = BufWriter::new(writer);

    debug!("Encoding message NeedsData");
    let message = nc_encode_data(&NC_NodeMessage::NeedsData(node_id))?;

    debug!("Sending message to server");
    nc_send_message(&mut buf_writer, message).await?;

    debug!("Receiving message from server");
    let (num_of_bytes_read, buffer) = nc_receive_message(&mut buf_reader).await?;

    debug!("Number of bytes read: {}", num_of_bytes_read);

    debug!("Decoding message");
    match nc_decode_data(&buffer)? {
        NC_ServerMessage::HasData(data) => {
            debug!("Received HasData");

            debug!("Processing data...");
            let processed_data = task::block_in_place(move || {
                nc_node.process_data_from_server(data).map_err(|e| NC_Error::NodeProcess(e))
            })?;

            debug!("Encoding message HasData");
            let message = nc_encode_data(&NC_NodeMessage::HasData((node_id, processed_data)))?;

            debug!("Send message back to server");
            nc_send_message(&mut buf_writer, message).await?;
            Ok(NC_JobStatus::Unfinished)
        }
        NC_ServerMessage::Waiting => {
            debug!("Received Waiting");
            Ok(NC_JobStatus::Waiting)
        }
        NC_ServerMessage::Finished => {
            debug!("Received Finished");
            Ok(NC_JobStatus::Finished)
        }
    }
}
