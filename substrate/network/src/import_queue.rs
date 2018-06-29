// Copyright 2017 Parity Technologies (UK) Ltd.
// This file is part of Polkadot.

// Polkadot is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Polkadot is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Polkadot.  If not, see <http://www.gnu.org/licenses/>.?

//! Blocks import queue.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use parking_lot::{Condvar, Mutex, RwLock};

use client::{BlockOrigin, BlockStatus, ImportResult};
use network::PeerId;
use runtime_primitives::generic::BlockId;
use runtime_primitives::traits::{Block as BlockT, Header as HeaderT, Zero};

use blocks::BlockData;
use chain::Client;
use error::{ErrorKind, Error};
use io::SyncIo;
use protocol::Protocol;
use service::{ExecuteInContext, Service};
use sync::ChainSync;

/// Blocks import queue API.
pub trait ImportQueue<B: BlockT>: Send + Sync where B::Header: HeaderT<Number=u64> {
	/// Start operating (if required).
	fn start(&self, service: Weak<Service<B>>, chain: Weak<Client<B>>) -> Result<(), Error>;
	/// Clear the queue when sync is restarting.
	fn clear(&self);
	/// Get queue status.
	fn status(&self) -> ImportQueueStatus<B>;
	/// Is block with given hash is currently in the queue.
	fn is_importing(&self, hash: &B::Hash) -> bool;
	/// Import bunch of blocks.
	fn import_blocks(&self, sync: &mut ChainSync<B>, io: &mut SyncIo, protocol: &Protocol<B>, blocks: (BlockOrigin, Vec<BlockData<B>>));
}

/// Import queue status. It isn't completely accurate.
pub struct ImportQueueStatus<B: BlockT> {
	/// Number of blocks that are currently in the queue.
	pub importing_count: usize,
	/// The number of the best block that was ever in the queue since start/last failure.
	pub best_importing_number: <<B as BlockT>::Header as HeaderT>::Number,
}

/// Blocks import queue that is importing blocks in the separate thread.
pub struct AsyncImportQueue<B: BlockT> {
	handle: Mutex<Option<::std::thread::JoinHandle<()>>>,
	data: Arc<AsyncImportQueueData<B>>,
}

/// Locks order: queue, queue_blocks, best_importing_number
struct AsyncImportQueueData<B: BlockT> {
	signal: Condvar,
	queue: Mutex<VecDeque<(BlockOrigin, Vec<BlockData<B>>)>>,
	queue_blocks: RwLock<HashSet<B::Hash>>,
	best_importing_number: RwLock<<<B as BlockT>::Header as HeaderT>::Number>,
	is_stopping: AtomicBool,
}

impl<B: BlockT> AsyncImportQueue<B> where B::Header: HeaderT<Number=u64> {
	pub fn new() -> Self {
		Self {
			handle: Mutex::new(None),
			data: Arc::new(AsyncImportQueueData::new()),
		}
	}
}

impl<B: BlockT> AsyncImportQueueData<B> where B::Header: HeaderT<Number=u64> {
	pub fn new() -> Self {
		Self {
			signal: Default::default(),
			queue: Mutex::new(VecDeque::new()),
			queue_blocks: RwLock::new(HashSet::new()),
			best_importing_number: RwLock::new(Zero::zero()),
			is_stopping: Default::default(),
		}
	}
}

impl<B: BlockT> ImportQueue<B> for AsyncImportQueue<B> where B::Header: HeaderT<Number=u64> {
	fn start(&self, service: Weak<Service<B>>, chain: Weak<Client<B>>) -> Result<(), Error> {
		debug_assert!(self.handle.lock().is_none());

		let qdata = self.data.clone();
		*self.handle.lock() = Some(::std::thread::Builder::new().name("ImportQueue".into()).spawn(move || {
			import_thread(service, chain, qdata)
		}).map_err(|err| Error::from(ErrorKind::Io(err)))?);
		Ok(())
	}

	fn clear(&self) {
		let mut queue = self.data.queue.lock();
		let mut queue_blocks = self.data.queue_blocks.write();
		let mut best_importing_number = self.data.best_importing_number.write();
		queue_blocks.clear();
		queue.clear();
		*best_importing_number = Zero::zero();
	}

	fn status(&self) -> ImportQueueStatus<B> {
		ImportQueueStatus {
			importing_count: self.data.queue_blocks.read().len(),
			best_importing_number: *self.data.best_importing_number.read(),
		}
	}

	fn is_importing(&self, hash: &B::Hash) -> bool {
		self.data.queue_blocks.read().contains(hash)
	}

	fn import_blocks(&self, _sync: &mut ChainSync<B>, _io: &mut SyncIo, _protocol: &Protocol<B>, blocks: (BlockOrigin, Vec<BlockData<B>>)) {
		let mut queue = self.data.queue.lock();
		let mut queue_blocks = self.data.queue_blocks.write();
		let mut best_importing_number = self.data.best_importing_number.write();
		let new_best_importing_number = blocks.1.last().and_then(|b| b.block.header.as_ref().map(|h| h.number().clone())).unwrap_or_default();
		queue_blocks.extend(blocks.1.iter().map(|b| b.block.hash.clone()));
		if new_best_importing_number > *best_importing_number {
			*best_importing_number = new_best_importing_number;
		}
		queue.push_back(blocks);
		self.data.signal.notify_one();
	}
}

impl<B: BlockT> Drop for AsyncImportQueue<B> {
	fn drop(&mut self) {
		if let Some(handle) = self.handle.lock().take() {
			self.data.is_stopping.store(true, Ordering::SeqCst);
			let _ = handle.join();
		}
	}
}

/// Blocks import thread.
fn import_thread<B: BlockT>(service: Weak<Service<B>>, chain: Weak<Client<B>>, qdata: Arc<AsyncImportQueueData<B>>) where B::Header: HeaderT<Number=u64> {
	loop {
		let new_blocks = {
			let mut queue_lock = qdata.queue.lock();
			if queue_lock.is_empty() {
				qdata.signal.wait(&mut queue_lock);
			}

			match queue_lock.pop_front() {
				Some(new_blocks) => new_blocks,
				None => break,
			}
		};

		if qdata.is_stopping.load(Ordering::SeqCst) {
			break;
		}

		match (service.upgrade(), chain.upgrade()) {
			(Some(service), Some(chain)) => {
				let blocks_hashes: Vec<B::Hash> = new_blocks.1.iter().map(|b| b.block.hash.clone()).collect();
				if !import_many_blocks(&mut SyncLink::Indirect(&*service), &*chain, Some(&*qdata), new_blocks) {
					break;
				}

				let mut queue_blocks = qdata.queue_blocks.write();
				for blocks_hash in blocks_hashes {
					queue_blocks.remove(&blocks_hash);
				}
			},
			_ => break,
		}
	}

	trace!(target: "sync", "Stopping import thread");
}

/// ChainSync link trait.
trait SyncLinkApi<B: BlockT> {
	/// Block imported.
	fn block_imported(&mut self, hash: &B::Hash, number: u64);
	/// Maintain sync.
	fn maintain_sync(&mut self);
	/// Disconnect from peer.
	fn disconnect(&mut self, peer_id: PeerId);
	/// Disconnect from peer and restart sync.
	fn disconnect_and_restart(&mut self, peer_id: PeerId);
	/// Restart sync.
	fn restart(&mut self);
}

/// Link with the ChainSync service.
enum SyncLink<'a, B: 'static + BlockT> where B::Header: HeaderT<Number=u64> {
	/// Indirect link (through service).
	Indirect(&'a Service<B>),
	/// Direct references are given.
	#[cfg(test)]
	Direct(&'a mut ChainSync<B>, &'a mut SyncIo, &'a Protocol<B>),
}

/// Block import successful result.
#[derive(Debug, PartialEq)]
enum BlockImportResult<H: ::std::fmt::Debug + PartialEq, N: ::std::fmt::Debug + PartialEq> {
	/// Block is not imported.
	NotImported(H, N),
	/// Imported known block.
	ImportedKnown(H, N),
	/// Imported unknown block.
	ImportedUnknown(H, N),
}

/// Block import error.
#[derive(Debug, PartialEq)]
enum BlockImportError {
	/// Disconnect from peer and continue import of next bunch of blocks.
	Disconnect(PeerId),
	/// Disconnect from peer and restart sync.
	DisconnectAndRestart(PeerId),
	/// Restart sync.
	Restart,
}

/// Import a bunch of blocks.
fn import_many_blocks<'a, B: BlockT>(
	link: &mut SyncLinkApi<B>,
	chain: &Client<B>,
	qdata: Option<&AsyncImportQueueData<B>>,
	blocks: (BlockOrigin, Vec<BlockData<B>>)
) -> bool where B::Header: HeaderT<Number=u64> {
	let (blocks_origin, blocks) = blocks;
	let count = blocks.len();
	let mut imported = 0;
	// Blocks in the response/drain should be in ascending order.
	for block in blocks {
		let import_result = import_single_block(chain, blocks_origin.clone(), block);
		let is_import_failed = import_result.is_err();
		imported += process_import_result(link, import_result);
		if is_import_failed {
			qdata.map(|qdata| *qdata.best_importing_number.write() = Zero::zero());
			return true;
		}

		if qdata.map(|qdata| qdata.is_stopping.load(Ordering::SeqCst)).unwrap_or_default() {
			return false;
		}
	}

	trace!(target: "sync", "Imported {} of {}", imported, count);
	link.maintain_sync();
	true
}

/// Single block import function.
fn import_single_block<B: BlockT>(
	chain: &Client<B>,
	block_origin: BlockOrigin,
	block: BlockData<B>
) -> Result<BlockImportResult<B::Hash, <<B as BlockT>::Header as HeaderT>::Number>, BlockImportError> {
	let origin = block.origin;
	let block = block.block;
	match (block.header, block.justification) {
		(Some(header), Some(justification)) => {
			let number = header.number().clone();
			let hash = header.hash();
			let parent = header.parent_hash().clone();

			// check whether the block is known before importing.
			match chain.block_status(&BlockId::Hash(hash)) {
				Ok(BlockStatus::InChain) => return Ok(BlockImportResult::NotImported(hash, number)),
				Ok(_) => {},
				Err(e) => {
					debug!(target: "sync", "Error importing block {}: {:?}: {:?}", number, hash, e);
					return Err(BlockImportError::Restart);
				}
			}

			let result = chain.import(
				block_origin,
				header,
				justification,
				block.body.map(|b| b.to_extrinsics()),
			);
			match result {
				Ok(ImportResult::AlreadyInChain) => {
					trace!(target: "sync", "Block already in chain {}: {:?}", number, hash);
					Ok(BlockImportResult::ImportedKnown(hash, number))
				},
				Ok(ImportResult::AlreadyQueued) => {
					trace!(target: "sync", "Block already queued {}: {:?}", number, hash);
					Ok(BlockImportResult::ImportedKnown(hash, number))
				},
				Ok(ImportResult::Queued) => {
					trace!(target: "sync", "Block queued {}: {:?}", number, hash);
					Ok(BlockImportResult::ImportedUnknown(hash, number))
				},
				Ok(ImportResult::UnknownParent) => {
					debug!(target: "sync", "Block with unknown parent {}: {:?}, parent: {:?}", number, hash, parent);
					Err(BlockImportError::Restart)
				},
				Ok(ImportResult::KnownBad) => {
					debug!(target: "sync", "Bad block {}: {:?}", number, hash);
					Err(BlockImportError::DisconnectAndRestart(origin)) //TODO: use persistent ID
				}
				Err(e) => {
					debug!(target: "sync", "Error importing block {}: {:?}: {:?}", number, hash, e);
					Err(BlockImportError::Restart)
				}
			}
		},
		(None, _) => {
			debug!(target: "sync", "Header {} was not provided by {} ", block.hash, origin);
			Err(BlockImportError::Disconnect(origin)) //TODO: use persistent ID
		},
		(_, None) => {
			debug!(target: "sync", "Justification set for block {} was not provided by {} ", block.hash, origin);
			Err(BlockImportError::Disconnect(origin)) //TODO: use persistent ID
		}
	}
}

/// Process single block import result.
fn process_import_result<'a, B: BlockT>(
	link: &mut SyncLinkApi<B>,
	result: Result<BlockImportResult<B::Hash, <<B as BlockT>::Header as HeaderT>::Number>, BlockImportError>
) -> usize where B::Header: HeaderT<Number=u64> {
	match result {
		Ok(BlockImportResult::NotImported(_, _)) => 0,
		Ok(BlockImportResult::ImportedKnown(hash, number)) => {
			link.block_imported(&hash, number);
			0
		},
		Ok(BlockImportResult::ImportedUnknown(hash, number)) => {
			link.block_imported(&hash, number);
			1
		},
		Err(BlockImportError::Disconnect(peer_id)) => {
			link.disconnect(peer_id);
			0
		},
		Err(BlockImportError::DisconnectAndRestart(peer_id)) => {
			link.disconnect_and_restart(peer_id);
			0
		},
		Err(BlockImportError::Restart) => {
			link.restart();
			0
		},
	}
}

impl<'a, B: 'static + BlockT> SyncLink<'a, B> where B::Header: HeaderT<Number=u64> {
	/// Execute closure with locked ChainSync.
	fn with_sync<F: Fn(&mut ChainSync<B>, &mut SyncIo, &Protocol<B>)>(&mut self, closure: F) {
		match *self {
			#[cfg(test)]
			SyncLink::Direct(ref mut sync, ref mut io, ref protocol) =>
				closure(*sync, *io, *protocol),
			SyncLink::Indirect(ref service) =>
				service.execute_in_context(move |io, protocol| {
					let mut sync = protocol.sync().write();
					closure(&mut *sync, io, protocol)
				}),
		}
	}
}

impl<'a, B: 'static + BlockT> SyncLinkApi<B> for SyncLink<'a, B> where B::Header: HeaderT<Number=u64> {
	fn block_imported(&mut self, hash: &B::Hash, number: u64) {
		self.with_sync(|sync, _, _| sync.block_imported(&hash, number))
	}
	
	fn maintain_sync(&mut self) {
		self.with_sync(|sync, io, protocol| sync.maintain_sync(io, protocol))
	}

	fn disconnect(&mut self, peer_id: PeerId) {
		self.with_sync(|_, io, _| io.disconnect_peer(peer_id))
	}

	fn disconnect_and_restart(&mut self, peer_id: PeerId) {
		self.with_sync(|sync, io, protocol| {
			io.disconnect_peer(peer_id);
			sync.restart(io, protocol);
		})
	}

	fn restart(&mut self) {
		self.with_sync(|sync, io, protocol| sync.restart(io, protocol))
	}
}

#[cfg(test)]
pub mod tests {
	use client;
	use message;
	use test_client::{self, TestClient};
	use test_client::runtime::{Block, Hash};
	use super::*;

	/// Blocks import queue that is importing blocks in the same thread.
	pub struct SyncImportQueue;

	impl<B: 'static + BlockT> ImportQueue<B> for SyncImportQueue where B::Header: HeaderT<Number=u64> {
		fn start(&self, _service: Weak<Service<B>>, _chain: Weak<Client<B>>) -> Result<(), Error> { Ok(()) }

		fn clear(&self) { }

		fn status(&self) -> ImportQueueStatus<B> {
			ImportQueueStatus {
				importing_count: 0,
				best_importing_number: Zero::zero(),
			}
		}

		fn is_importing(&self, _hash: &B::Hash) -> bool {
			false
		}

		fn import_blocks(&self, sync: &mut ChainSync<B>, io: &mut SyncIo, protocol: &Protocol<B>, blocks: (BlockOrigin, Vec<BlockData<B>>)) {
			let chain = protocol.chain();
			import_many_blocks(&mut SyncLink::Direct(sync, io, protocol), chain, None, blocks);
		}
	}

	#[derive(Default)]
	struct TestLink {
		imported: usize,
		maintains: usize,
		disconnects: usize,
		restarts: usize,
	}

	impl TestLink {
		fn total(&self) -> usize {
			self.imported + self.maintains + self.disconnects + self.restarts
		}
	}

	impl<B: BlockT> SyncLinkApi<B> for TestLink {
		fn block_imported(&mut self, _hash: &B::Hash, _number: u64) { self.imported += 1; }
		fn maintain_sync(&mut self) { self.maintains += 1; }
		fn disconnect(&mut self, _peer_id: PeerId) { self.disconnects += 1; }
		fn disconnect_and_restart(&mut self, _peer_id: PeerId) { self.disconnects += 1; self.restarts += 1; }
		fn restart(&mut self) { self.restarts += 1; }
	}

	fn prepare_good_block() -> (client::Client<test_client::Backend, test_client::Executor, Block>, Hash, u64, BlockData<Block>) {
		let client = test_client::new();
		let block = client.new_block().unwrap().bake().unwrap();
		client.justify_and_import(BlockOrigin::File, block).unwrap();

		let (hash, number) = (client.block_hash(1).unwrap().unwrap(), 1);
		let block = message::BlockData::<Block> {
			hash: client.block_hash(1).unwrap().unwrap(),
			header: client.header(&BlockId::Number(1)).unwrap(),
			body: None,
			receipt: None,
			message_queue: None,
			justification: client.justification(&BlockId::Number(1)).unwrap(),
		};

		(client, hash, number, BlockData { block, origin: 0 })
	}

	#[test]
	fn import_single_good_block_works() {
		let (_, hash, number, block) = prepare_good_block();
		assert_eq!(import_single_block(&test_client::new(), BlockOrigin::File, block), Ok(BlockImportResult::ImportedUnknown(hash, number)));
	}

	#[test]
	fn import_single_good_known_block_is_ignored() {
		let (client, hash, number, block) = prepare_good_block();
		assert_eq!(import_single_block(&client, BlockOrigin::File, block), Ok(BlockImportResult::NotImported(hash, number)));
	}

	#[test]
	fn import_single_good_block_without_header_fails() {
		let (_, _, _, mut block) = prepare_good_block();
		block.block.header = None;
		assert_eq!(import_single_block(&test_client::new(), BlockOrigin::File, block), Err(BlockImportError::Disconnect(0)));
	}

	#[test]
	fn import_single_good_block_without_justification_fails() {
		let (_, _, _, mut block) = prepare_good_block();
		block.block.justification = None;
		assert_eq!(import_single_block(&test_client::new(), BlockOrigin::File, block), Err(BlockImportError::Disconnect(0)));
	}

	#[test]
	fn process_import_result_works() {
		let mut link = TestLink::default();
		assert_eq!(process_import_result::<Block>(&mut link, Ok(BlockImportResult::NotImported(Default::default(), 0))), 0);
		assert_eq!(link.total(), 0);

		let mut link = TestLink::default();
		assert_eq!(process_import_result::<Block>(&mut link, Ok(BlockImportResult::ImportedKnown(Default::default(), 0))), 0);
		assert_eq!(link.total(), 1);
		assert_eq!(link.imported, 1);

		let mut link = TestLink::default();
		assert_eq!(process_import_result::<Block>(&mut link, Ok(BlockImportResult::ImportedUnknown(Default::default(), 0))), 1);
		assert_eq!(link.total(), 1);
		assert_eq!(link.imported, 1);

		let mut link = TestLink::default();
		assert_eq!(process_import_result::<Block>(&mut link, Err(BlockImportError::Disconnect(0))), 0);
		assert_eq!(link.total(), 1);
		assert_eq!(link.disconnects, 1);

		let mut link = TestLink::default();
		assert_eq!(process_import_result::<Block>(&mut link, Err(BlockImportError::DisconnectAndRestart(0))), 0);
		assert_eq!(link.total(), 2);
		assert_eq!(link.disconnects, 1);
		assert_eq!(link.restarts, 1);

		let mut link = TestLink::default();
		assert_eq!(process_import_result::<Block>(&mut link, Err(BlockImportError::Restart)), 0);
		assert_eq!(link.total(), 1);
		assert_eq!(link.restarts, 1);
	}

	#[test]
	fn import_many_blocks_stops_when_stopping() {
		let (_, _, _, block) = prepare_good_block();
		let qdata = AsyncImportQueueData::new();
		qdata.is_stopping.store(true, Ordering::SeqCst);
		assert!(!import_many_blocks(&mut TestLink::default(), &test_client::new(), Some(&qdata), (BlockOrigin::File, vec![block.clone(), block])));
	}
}
