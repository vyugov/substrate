use std::{
	collections::VecDeque,
	fmt::Debug,
	marker::PhantomData,
	time::{Duration, Instant},
};

use client::{
	backend::Backend, blockchain::HeaderBackend, error::Error as ClientError,  error::Result,
	BlockchainEvents, CallExecutor, Client,
};
//use codec::{Decode, Encode};
use consensus_common::SelectChain;
use futures03::{ prelude::*, stream::Fuse, core_reexport::pin::Pin,Poll,};
use inherents::InherentDataProviders;
use sr_primitives::traits::Block as BlockT;
use tokio_timer::Interval;
use futures03::task::Context;
use crate::Error;

pub struct PeriodicStream<Block: BlockT, S, M> {
	incoming: Fuse<S>,
	check_pending: Interval,
	ready: VecDeque<M>,
	_phantom: PhantomData<Block>,
}


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

		while let Some(_p) = match self
			.check_pending
			.poll_next(cx)
			
			 {
				 Poll::Ready(dat) => dat,
				 Poll::Pending => return 	 futures03::Poll::Pending,
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


