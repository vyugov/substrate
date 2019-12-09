// pub fn run_key_gen<B, E, Block, N, RA>(
// 	keystore: KeyStorePtr,
// 	client: Arc<Client<B, E, Block, RA>>,
// 	network: N,
// 	backend: Arc<B>,
// ) -> ClientResult<impl Future<Output = Result<(), ()>> + Send + 'static>
// where
// 	B: Backend<Block, Blake2Hasher> + 'static,
// 	E: CallExecutor<Block, Blake2Hasher> + 'static + Send + Sync,
// 	Block: BlockT<Hash = H256> + Unpin,
// 	Block::Hash: Ord,
// 	N: Network<Block> + Send + Sync + Unpin + 'static,
// 	N::In: Send + 'static,
// 	RA: Send + Sync + 'static,
// {
// }
