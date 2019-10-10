use std::{
	collections::VecDeque,
	fmt::Debug,
	marker::Unpin,
	pin::Pin,
	sync::Arc,
	time::{Duration, Instant},
};

use codec::{Decode, Encode};

use futures::prelude::{Sink as Sink01, Stream as Stream01};
use futures03::compat::{Compat, Compat01As03};
use futures03::prelude::{Future, Sink, Stream, TryStream};
use futures03::sink::SinkExt;
use futures03::stream::{FilterMap, Fuse, StreamExt, TryStreamExt};
use futures03::task::{Context, Poll};

use tokio02::timer::Interval;

pub struct PeriodicStream<S, M>
where
	S: Stream<Item = M> + Unpin,
{
	incoming: Fuse<S>,
	check_pending: Interval,
	ready: VecDeque<M>,
}

impl<S, M> PeriodicStream<S, M>
where
	S: Stream<Item = M> + Unpin,
{
	pub fn new(stream: S) -> Self {
		let dur = Duration::from_secs(5);

		Self {
			incoming: stream.fuse(),
			check_pending: Interval::new_interval(dur),
			ready: VecDeque::new(),
		}
	}
}

impl<S, M> Stream for PeriodicStream<S, M>
where
	S: Stream<Item = M> + Unpin,
	M: Debug + Unpin,
{
	type Item = S::Item;

	fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
		loop {
			match self.incoming.poll_next_unpin(cx) {
				Poll::Ready(None) => return Poll::Ready(None),
				Poll::Ready(Some(input)) => {
					let ready = &mut self.ready;
					ready.push_back(input);
				}
				Poll::Pending => break,
			}
		}

		while let Some(_) = match self.check_pending.poll_next_unpin(cx) {
			Poll::Ready(instant) => instant,
			Poll::Pending => return Poll::Pending,
		} {}

		if let Some(ready) = self.ready.pop_front() {
			return Poll::Ready(Some(ready));
		}

		if self.incoming.is_done() {
			Poll::Ready(None)
		} else {
			Poll::Pending
		}
	}
}
