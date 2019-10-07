use std::{
	collections::VecDeque,
	fmt::Debug,
	marker::PhantomData,
	time::{Duration, Instant},
};

//use client::{
//	backend::Backend, blockchain::HeaderBackend, error::Error as ClientError, error::Result,
//	BlockchainEvents, CallExecutor, Client,
//};
//use codec::{Decode, Encode};
//use consensus_common::SelectChain;

#[cfg(feature = "upgraded")]
use futures03::{ prelude::*, stream::Fuse, };//channel::mpsc};


#[cfg(not(feature = "upgraded"))]
use futures::{ prelude::*, stream::Fuse, sync::mpsc};


//use inherents::InherentDataProviders;
//use log::{debug, info, warn};
use sr_primitives::traits::{Block as BlockT,  NumberFor, ProvideRuntimeApi};//DigestFor,
use tokio_timer::Interval;

use crate::Error;

pub struct PeriodicStream<Block: BlockT, S, M> 
{
	incoming: Fuse<S>,
	check_pending: Interval,
	ready: VecDeque<M>,
	_phantom: PhantomData<Block>,
}

#[cfg(not(feature = "upgraded"))]
impl<Block, S, M> PeriodicStream<Block, S, M>
where
	Block: BlockT,
	S: Stream<Item = M, Error = Error>,
	M: Debug,
{
	pub fn new(stream: S) -> Self {
		let now = Instant::now();
		let dur = Duration::from_secs(5);
		let check_pending = Interval::new(now + dur, dur);

		Self {
			incoming: stream.fuse(),
			check_pending,
			ready: VecDeque::new(),
			_phantom: PhantomData,
		}
	}
}

#[cfg(feature = "upgraded")]
impl<Block, S, M> PeriodicStream<Block, S, M>
where
	Block: BlockT+Unpin,
	S: Stream<Item = M>+Unpin,
	M: Debug+Unpin,
{
	pub fn new(stream: S) -> Self {
		let now = Instant::now();
		let dur = Duration::from_secs(5);
		let check_pending = Interval::new(now + dur, dur);

		Self {
			incoming: stream.fuse(),
			check_pending,
			ready: VecDeque::new(),
			_phantom: PhantomData,
		}
	}
}

#[cfg(not(feature = "upgraded"))]
impl<Block, S, M> Stream for PeriodicStream<Block, S, M>
where
	Block: BlockT,
	S: Stream<Item = M, Error = Error>,
	M: Debug,
{
	type Item = M;
	type Error = Error;

	fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
		loop {
			match self.incoming.poll()? {
				Async::Ready(None) => return Ok(Async::Ready(None)),
				Async::Ready(Some(input)) => {
					let ready = &mut self.ready;
					ready.push_back(input);
				}
				Async::NotReady => break,
			}
		}

		while let Async::Ready(Some(p)) = self
			.check_pending
			.poll()
			.map_err(|e| Error::Network("pending err".to_string()))?
		{}

		if let Some(ready) = self.ready.pop_front() {
			return Ok(Async::Ready(Some(ready)));
		}

		if self.incoming.is_done() {
			println!("worker incoming done");
			Ok(Async::Ready(None))
		} else {
			println!("worker incoming not ready");
			Ok(Async::NotReady)
		}
	}
}

use futures::stream::Stream as fstream;


#[cfg(feature = "upgraded")]
use std::{pin::Pin, task::Context, task::Poll};

#[cfg(feature = "upgraded")]
impl<Block, S: Stream, M> Stream for PeriodicStream<Block, S, M>
where
	Block: BlockT+Unpin,
	S: futures03::Stream<Item = M>+Unpin,
	M: Debug+Unpin,
{
	type Item = M;
	//type Error = Error;

	 fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>>
	{
		loop {
			 match Pin::new(self.incoming.get_mut()).poll_next(cx)
			   
			 {
		      	Poll::Ready(None)=> return futures03::Poll::Ready(None),
				Poll::Ready(Some(input)) => {
					let ready = &mut self.ready;
					ready.push_back(input);
				}
				Poll::Pending => break,
			}
		}

		while let futures::Async::Ready(Some(_p)) = match self
			.check_pending
			.poll()
			.map_err(|_e| Error::Network("pending err".to_string()))
			 {
				 Ok(dat) => dat,
				 Err(_) => return 	 futures03::Poll::Pending,
			 }
		{}

		if let Some(ready) = self.ready.pop_front() {
			return futures03::Poll::Ready(Some(ready));
		}

		if self.incoming.is_done() {
			println!("worker incoming done");
			futures03::Poll::Ready(None)
		} else {
			println!("worker incoming not ready");
			 futures03::Poll::Pending
		}
	}
}



