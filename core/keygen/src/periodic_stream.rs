use std::{
	collections::VecDeque,
	fmt::Debug,
	marker::Unpin,
	pin::Pin,
	sync::Arc,
	time::{Duration, Instant},
};

use codec::{Decode, Encode};

use futures03::compat::{Compat, Compat01As03};
use futures03::prelude::{Stream, TryStream};
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
		println!("in ps");
		loop {
			match self.incoming.poll_next_unpin(cx) {
				Poll::Ready(None) => break,
				Poll::Ready(Some(input)) => {
					println!("input {:?}", input);
					let ready = &mut self.ready;
					ready.push_back(input);
				}
				Poll::Pending => break,
			}
		}

		if let Some(_) = match self.check_pending.poll_next(cx) {
			Poll::Ready(instant) => {
				println!("ready {:?}", instant);
				instant
			}
			Poll::Pending => {
				return {
					println!("pending");
					Poll::Pending
				}
			}
		} {}

		if let Some(ready) = self.ready.pop_front() {
			println!("return");
			return Poll::Ready(Some(ready));
		}

		if self.incoming.is_done() {
			println!("finished");
			Poll::Ready(None)
		} else {
			Poll::Pending
		}
	}
}

#[cfg(test)]
mod test {

	use futures03::{
		future::{self, Future, FutureExt, TryFutureExt},
		stream,
	};
	use tokio::runtime::Runtime as Runtime01;
	use tokio02::runtime::Runtime;

	use super::*;

	#[test]
	fn test_stream() {
		let rt = Runtime::new().unwrap();

		let mut rt01 = Runtime01::new().unwrap();

		// let mut i = 0;
		// let f = future::poll_fn(move |_| {
		// 	i += 1;
		// 	println!("{:?}", i);
		// 	if i == 10000 {
		// 		return Poll::Ready(None);
		// 	}
		// 	if i % 10 == 0 {
		// 		Poll::Ready(Some(i))
		// 	} else {
		// 		Poll::Pending
		// 	}
		// });

		struct F<S>
		where
			S: Stream<Item = u8>,
		{
			i: S,
		};
		impl<S> Future for F<S>
		where
			S: Stream<Item = u8> + Unpin,
		{
			type Output = ();

			fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
				println!("in future");

				// while let Poll::Ready(Some(item)) = self.i.poll_next_unpin(cx) {
				// 	println!("get item {:?}", item);
				// }
				// Poll::Pending

				loop {
					match self.i.poll_next_unpin(cx) {
						Poll::Ready(Some(item)) => {
							println!("item {:?}", item);
						}
						Poll::Ready(None) => return Poll::Ready(()),
						Poll::Pending => return Poll::Pending,
					}
				}
			}
		}

		let s = stream::repeat(1u8).take(10);
		let ps = PeriodicStream::<_, u8>::new(s);
		let f = F { i: ps };

		rt.block_on(f);
		// rt01.block_on(f.map(|_| -> Result<(), ()> { Ok(()) }).compat())
		// .unwrap();

		println!("test");
	}
}
