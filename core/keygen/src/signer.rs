use std::{collections::VecDeque, marker::PhantomData, sync::Arc};

use codec::{Decode, Encode};
use curv::GE;

#[cfg(not(feature = "upgraded"))]
use futures::prelude::*;

#[cfg(feature = "upgraded")]
use futures03::prelude::*;

#[cfg(feature = "upgraded")]
use futures03::Poll;

#[cfg(feature = "upgraded")]
use futures03::core_reexport::pin::Pin;


use log::{debug, error, info, warn};
use multi_party_ecdsa::protocols::multi_party_ecdsa::gg_2018::party_i::{Keys, Parameters};
use tokio_executor::DefaultExecutor;
use tokio_timer::Interval;

use client::{
	backend::Backend, error::Error as ClientError, error::Result as ClientResult, BlockchainEvents,
	CallExecutor, Client,
};
use consensus_common::SelectChain;
use inherents::InherentDataProviders;
use network::PeerId;
use primitives::{Blake2Hasher, H256};
use sr_primitives::generic::BlockId;
use sr_primitives::traits::{Block as BlockT, NumberFor, ProvideRuntimeApi};

use super::{
	ConfirmPeersMessage, Environment, Error, GossipMessage, KeyGenMessage, Message,
	MessageWithSender, Network, PeerIndex, SignMessage,
};
use crate::communication::MWSStream;
use crate::communication::MWSSink;

#[cfg(not(feature = "upgraded"))]
struct Buffered<S: Sink> {
	inner: S,
	buffer: VecDeque<S::SinkItem>,
}


#[cfg(not(feature = "upgraded"))]
impl<S: Sink> Buffered<S> {
	fn new(inner: S) -> Buffered<S> {
		Buffered {
			buffer: VecDeque::new(),
			inner,
		}
	}

	// push an item into the buffered sink.
	// the sink _must_ be driven to completion with `poll` afterwards.
	fn push(&mut self, item: S::SinkItem) {
		self.buffer.push_back(item);
	}

	// returns ready when the sink and the buffer are completely flushed.
	fn poll(&mut self) -> Poll<(), S::SinkError> {
		let polled = self.schedule_all()?;

		match polled {
			Async::Ready(()) => self.inner.poll_complete(),
			Async::NotReady => {
				self.inner.poll_complete()?;
				Ok(Async::NotReady)
			}
		}
	}

	fn schedule_all(&mut self) -> Poll<(), S::SinkError> {
		while let Some(front) = self.buffer.pop_front() {
			match self.inner.start_send(front) {
				Ok(AsyncSink::Ready) => continue,
				Ok(AsyncSink::NotReady(front)) => {
					self.buffer.push_front(front);
					break;
				}
				Err(e) => return Err(e),
			}
		}

		if self.buffer.is_empty() {
			Ok(Async::Ready(()))
		} else {
			Ok(Async::NotReady)
		}
	}
}


#[cfg(feature = "upgraded")]
struct Buffered<A,S>
where S: futures03::sink::Sink<A, Error = Error>+Unpin
 {
	inner:S,
	buffer: VecDeque<A>,
}

#[cfg(feature = "upgraded")]
impl<A,S>  Unpin for  Buffered<A,S>
where S: futures03::sink::Sink<A, Error = Error>+Unpin
{}

#[cfg(feature = "upgraded")]
impl<SinkItem,S> Buffered<SinkItem,S> 
where S: futures03::sink::Sink<SinkItem, Error = Error>+Unpin,
{
	fn new(inner: S) -> Buffered<SinkItem,S>
	 {
		Buffered {
			buffer: VecDeque::new(),
			inner,
		}
	}

	// push an item into the buffered sink.
	// the sink _must_ be driven to completion with `poll` afterwards.
	fn push(&mut self, item: SinkItem) {
		self.buffer.push_back(item);
	}
	// returns ready when the sink and the buffer are completely flushed.
	fn poll(mut self: Pin<&mut Self>,cx: &mut std::task::Context<'_>) -> Poll<()> {
		let polled =  (*self).schedule_all(cx);

		let respoll=match polled {
			futures03::Poll::Ready(()) => Pin::new((&mut (*self).inner)).poll_flush(cx),
			futures03::Poll::Pending => {
				Pin::new((&mut (*self).inner)).poll_flush(cx);
				futures03::Poll::Pending 
			}
		};
		match respoll
		{
        Poll::Pending => Poll::Pending,
		Poll::Ready(Ok(_)) =>Poll::Ready(()),
		Poll::Ready(Err(_)) =>Poll::Pending,
		}
	}

	fn schedule_all(&mut self,cx: &mut std::task::Context<'_>) -> Poll<()> {
		match Pin::new(&mut self.inner).poll_ready(cx)
		{
			Poll::Pending => return Poll::Pending,
			Poll::Ready(Ok( () )) => {},
			Poll::Ready(Err(_)) => return Poll::Pending,
		}
		while let Some(front) = self.buffer.pop_front() {
              info!("Some in FRONT {:?}",self.buffer.len());
			match Pin::new(&mut self.inner).start_send(front) {
				Ok(()) => {
             match Pin::new(&mut self.inner).poll_ready(cx)
		 {
			Poll::Pending => return Poll::Pending,
			Poll::Ready(Ok( () )) => continue,
			Poll::Ready(Err(_)) => return Poll::Pending, //todo::convert
		 }

				},
				Err(_)=> {
				//	self.buffer.push_front(front);
					break;
				}
			}
			
		}

		if self.buffer.is_empty() {
			Poll::Ready(())
		} else {
			Poll::Pending
		}
	}
}





#[cfg(not(feature = "upgraded"))]
pub(crate) struct Signer<B, E, Block: BlockT, N: Network<Block>, RA, In, Out>
where
	In: Stream<Item = MessageWithSender, Error = Error>,
	Out: Sink<SinkItem = MessageWithSender, SinkError = Error>,
{
	env: Arc<Environment<B, E, Block, N, RA>>,
	global_in: In,
	global_out: Buffered<Out>,
}

#[cfg(feature = "upgraded")]
pub(crate) struct Signer<B, E, Block: BlockT, N: Network<Block>, RA, In, Out>
where
	In: Stream<Item = MessageWithSender>,
	Out: Sink<MessageWithSender, Error = Error>+Unpin,
{
	env: Arc<Environment<B, E, Block, N, RA>>,
	global_in: In,
	global_out: Buffered<MessageWithSender,Out>,
}


impl<B, E, Block, N, RA, In, Out> Signer<B, E, Block, N, RA, In, Out>
where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + Send + Sync + 'static,
	Block: BlockT<Hash = H256>,
	Block::Hash: Ord,
	N: Network<Block> + Sync,
	N::In: Send + 'static,
	RA: Send + Sync + 'static,
	In: MWSStream,
	Out: MWSSink,
{
	pub fn new(env: Arc<Environment<B, E, Block, N, RA>>, global_in: In, global_out: Out) -> Self {
		Signer {
			env,
			global_in,
			global_out: Buffered::new(global_out),
		}
	}

	fn generate_shared_keys(&mut self) {
		info!("generate_shared_keys");
		let mut state = self.env.state.write();
		if state.confirmations.vss != state.confirmations.secret_share
			|| state.confirmations.vss != self.env.config.players - 1
			|| state.confirmations.secret_share != self.env.config.players - 1
			|| state.complete
		{
			return;
		}

		let params = Parameters {
			threshold: self.env.config.threshold,
			share_count: self.env.config.players,
		};

		let mut key: Keys;
		if let Some(vk)= state.local_key.clone()
		{
			key=vk;
		}
		else
       { 
      return;
       }


		let vsss = state.vsss.values().cloned().collect::<Vec<_>>();

		let secret_shares = state.secret_shares.values().cloned().collect::<Vec<_>>();

		let points = state.decommits.values().map(|x| x.y_i).collect::<Vec<_>>();

		let (shared_keys, proof) = key
			.phase2_verify_vss_construct_keypair_phase3_pok_dlog(
				&params,
				points.as_slice(),
				secret_shares.as_slice(),
				vsss.as_slice(),
				key.party_index + 1,
			)
			.unwrap();

		println!("shared keys: {:?} proof: {:?}", shared_keys, proof);

		let i = key.party_index as PeerIndex;
		state.proofs.insert(i, proof.clone());
		state.complete = true;
		state.shared_keys = Some(shared_keys);

		drop(state);

		let proof_msg = KeyGenMessage::Proof(i, proof);
		self.global_out.push((Message::KeyGen(proof_msg), None));
	}

	fn handle_incoming(&mut self, msg: &Message, sender: &Option<PeerId>) {
		let players = self.env.config.players;
        info!("Incoming: MSG {:?}",&msg);
		match msg {
			Message::ConfirmPeers(ConfirmPeersMessage::Confirming(from_index, hash)) => {
				let sender = sender.clone().unwrap();

				let validator = self.env.bridge.validator.inner.read();
				let index = validator.get_peer_index(&sender) as PeerIndex;
				let our_hash = validator.get_peers_hash();
				if *from_index == index && *hash == our_hash {
					self.global_out.push((
						Message::ConfirmPeers(ConfirmPeersMessage::Confirmed(
							validator.local_string_id(),
						)),
						Some(sender),
					));
				}
			}
			Message::ConfirmPeers(ConfirmPeersMessage::Confirmed(sender_string_id)) => {
				let sender = sender.clone().unwrap();

				let mut state = self.env.state.write();
				state.confirmations.peer += 1;

				let mut validator = self.env.bridge.validator.inner.write();
				validator.set_peer_generating(&sender);

				if state.confirmations.peer == players - 1 {
					validator.set_local_generating();
					let local_index = validator.get_local_index();
					drop(validator);

					let key = Keys::create(local_index);
					let (commit, decommit) = key.phase1_broadcast_phase3_proof_of_correct_key();

					let index = local_index as PeerIndex;
					state.commits.insert(index, commit.clone());
					state.decommits.insert(index, decommit.clone());

					state.local_key = Some(key);
					drop(state);

					let cad_msg = KeyGenMessage::CommitAndDecommit(index, commit, decommit);

					self.global_out.push((Message::KeyGen(cad_msg), None));
				}
			}
			Message::KeyGen(KeyGenMessage::CommitAndDecommit(from_index, commit, decommit)) => {
				let mut state = self.env.state.write();
				state.confirmations.com_decom += 1;

				state.commits.insert(*from_index, commit.clone());
				state.decommits.insert(*from_index, decommit.clone());

				if state.confirmations.com_decom == players - 1 {
					let params = Parameters {
						threshold: self.env.config.threshold,
						share_count: self.env.config.players,
					};

					let key: Keys = state.local_key.clone().unwrap();

					let commits = state.commits.values().cloned().collect::<Vec<_>>();
					let decommits = state.decommits.values().cloned().collect::<Vec<_>>();
					let us:usize=params.share_count.into();
                     if decommits.len()==us
					 {

					 
					let (vss, secret_shares, index) = key
						.phase1_verify_com_phase3_verify_correct_key_phase2_distribute(
							&params,
							decommits.as_slice(),
							commits.as_slice(),
						)
						.unwrap();

					let share = secret_shares[index].clone();
					state.vsss.insert(index as PeerIndex, vss.clone());
					state.secret_shares.insert(index as PeerIndex, share);

					drop(state);

					self.global_out.push((
						Message::KeyGen(KeyGenMessage::VSS(index as PeerIndex, vss)),
						None,
					));

					let validator = self.env.bridge.validator.inner.read();

					for (i, &ss) in secret_shares.iter().enumerate() {
						if i != index {
							let ss_msg = KeyGenMessage::SecretShare(index as PeerIndex, ss);
							info!("Index: {:?}",i);
							let peer = validator.get_peer_id_by_index(i).unwrap();
							self.global_out.push((Message::KeyGen(ss_msg), Some(peer)));
						}
					}
					}
				}
			}
			Message::KeyGen(KeyGenMessage::VSS(from_index, vss)) => {
				let mut state = self.env.state.write();
				state.confirmations.vss += 1;

				state.vsss.insert(*from_index, vss.clone());
			}
			Message::KeyGen(KeyGenMessage::SecretShare(from_index, ss)) => {
				let mut state = self.env.state.write();
				state.confirmations.secret_share += 1;

				state.secret_shares.insert(*from_index, ss.clone());
			}
			Message::KeyGen(KeyGenMessage::Proof(from_index, proof)) => {
				let mut state = self.env.state.write();
				state.confirmations.proof += 1;

				state.proofs.insert(*from_index, proof.clone());

				let mut validator = self.env.bridge.validator.inner.write();
				let sender = validator
					.get_peer_id_by_index(*from_index as usize)
					.unwrap();
				validator.set_peer_complete(&sender);

				if state.confirmations.proof == players - 1 {
					let params = Parameters {
						threshold: self.env.config.threshold,
						share_count: self.env.config.players,
					};

					let proofs = state.proofs.values().cloned().collect::<Vec<_>>();
					let points = state.decommits.values().map(|x| x.y_i).collect::<Vec<_>>();

					if proofs.len() != players as usize || points.len() != players as usize {
						return;
					}

					if Keys::verify_dlog_proofs(&params, proofs.as_slice(), points.as_slice())
						.is_ok()
					{
						info!("Key generation complete");
						validator.set_local_complete();
					} else {
						// reset everything?
						error!("Key generation failed");
					}
				}
			}
			Message::Sign(_) => {}
			_ => {}
		}
	}
}

impl<B, E, Block, N, RA, In, Out> Future for Signer<B, E, Block, N, RA, In, Out>
where
	B: Backend<Block, Blake2Hasher> + 'static,
	E: CallExecutor<Block, Blake2Hasher> + Send + Sync + 'static,
	Block: BlockT<Hash = H256>,
	Block::Hash: Ord,
	N: Network<Block> + Sync,
	N::In: Send + 'static,
	RA: Send + Sync + 'static,
	In: MWSStream,
	Out: MWSSink,
{
    #[cfg(feature = "upgraded")]
	type Output=Result<(),Error>;


    #[cfg(not(feature = "upgraded"))]
	type Item = ();

	#[cfg(not(feature = "upgraded"))]
	type Error = Error;


	#[cfg(not(feature = "upgraded"))]
	fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
		while let Async::Ready(Some(item)) = self.global_in.poll()? {
			let (msg, sender) = item;
			self.handle_incoming(&msg, &sender);
		}

		// send messages generated by `handle_incoming`
		self.global_out.poll()?;
		{
			let state = self.env.state.read();
			println!("state: {:?}", state.confirmations);
		}
		self.generate_shared_keys();

		Ok(Async::NotReady)
	}

    #[cfg(feature = "upgraded")]
	fn poll(mut self: Pin<&mut Self>,cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
		while let Poll::Ready(Some(item)) = Pin::new(&mut self.global_in).poll_next(cx) {
			let (msg, sender) = item;
			self.handle_incoming(&msg, &sender);
		}

		// send messages generated by `handle_incoming`
		 Pin::new(&mut self.global_out).poll(cx);
		{
			let state = self.env.state.read();
			println!("state: {:?}", state.confirmations);
		}
		self.generate_shared_keys();

		Poll::Pending
	}

}
