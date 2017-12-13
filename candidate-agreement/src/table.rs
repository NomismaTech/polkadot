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
// along with Polkadot.  If not, see <http://www.gnu.org/licenses/>.

//! The statement table.
//!
//! This stores messages other validators issue about candidates.
//!
//! These messages are used to create a proposal submitted to a BFT consensus process.
//!
//! Proposals are formed of sets of candidates which have the requisite number of
//! validity and availability votes.
//!
//! Each parachain is associated with two sets of validators: those which can
//! propose and attest to validity of candidates, and those who can only attest
//! to availability.

use std::collections::hash_map::{HashMap, Entry};
use std::hash::Hash;
use std::fmt::Debug;

/// Statements circulated among peers.
#[derive(PartialEq, Eq, Debug)]
pub enum Statement<C: Context + ?Sized> {
	/// Broadcast by a validator to indicate that this is his candidate for
	/// inclusion.
	///
	/// Broadcasting two different candidate messages per round is not allowed.
	Candidate(C::Candidate),
	/// Broadcast by a validator to attest that the candidate with given digest
	/// is valid.
	Valid(C::Digest),
	/// Broadcast by a validator to attest that the auxiliary data for a candidate
	/// with given digest is available.
	Available(C::Digest),
	/// Broadcast by a validator to attest that the candidate with given digest
	/// is invalid.
	Invalid(C::Digest),
}

/// A signed statement.
#[derive(PartialEq, Eq, Debug)]
pub struct SignedStatement<C: Context + ?Sized> {
	/// The statement.
	pub statement: Statement<C>,
	/// The signature.
	pub signature: C::Signature,
}

/// Context for the statement table.
pub trait Context {
	/// A validator ID
	type ValidatorId: Hash + Eq + Clone + Debug;
	/// The digest (hash or other unique attribute) of a candidate.
	type Digest: Hash + Eq + Clone + Debug;
    /// Candidate type.
	type Candidate: Ord + Clone + Eq + Debug;
	/// The group ID type
	type GroupId: Hash + Eq + Clone + Debug + Ord;
	/// A signature type.
	type Signature: Clone + Eq + Debug;

	/// get the digest of a candidate.
	fn candidate_digest(&self, candidate: &Self::Candidate) -> Self::Digest;

	/// get the group of a candidate.
	fn candidate_group(&self, candidate: &Self::Candidate) -> Self::GroupId;

	/// Whether a validator is a member of a group.
	/// Members are meant to submit candidates and vote on validity.
	fn is_member_of(&self, validator: &Self::ValidatorId, group: &Self::GroupId) -> bool;

	/// Whether a validator is an availability guarantor of a group.
	/// Guarantors are meant to vote on availability for candidates submitted
	/// in a group.
	fn is_availability_guarantor_of(
		&self,
		validator: &Self::ValidatorId,
		group: &Self::GroupId,
	) -> bool;

	// recover signer of statement.
	fn statement_signer(
		&self,
		statement: &SignedStatement<Self>,
	) -> Option<Self::ValidatorId>;

	// requisite number of votes for validity and availability respectively from a group.
	fn requisite_votes(&self, group: &Self::GroupId) -> (usize, usize);
}

/// Misbehavior: voting more than one way on candidate validity.
///
/// Since there are three possible ways to vote, a double vote is possible in
/// three possible combinations.
#[derive(PartialEq, Eq, Debug)]
pub enum ValidityDoubleVote<C: Context> {
	/// Implicit vote by issuing and explicity voting validity.
	IssuedAndValidity((C::Candidate, C::Signature), (C::Digest, C::Signature)),
	/// Implicit vote by issuing and explicitly voting invalidity
	IssuedAndInvalidity((C::Candidate, C::Signature), (C::Digest, C::Signature)),
	/// Direct votes for validity and invalidity
	ValidityAndInvalidity(C::Digest, C::Signature, C::Signature),
}

/// Misbehavior: declaring multiple candidates.
#[derive(PartialEq, Eq, Debug)]
pub struct MultipleCandidates<C: Context> {
	/// The first candidate seen.
	pub first: (C::Candidate, C::Signature),
	/// The second candidate seen.
	pub second: (C::Candidate, C::Signature),
}

/// Misbehavior: submitted statement for wrong group.
#[derive(PartialEq, Eq, Debug)]
pub struct UnauthorizedStatement<C: Context> {
	/// A signed statement which was submitted without proper authority.
	pub statement: SignedStatement<C>,
}

/// Different kinds of misbehavior. All of these kinds of malicious misbehavior
/// are easily provable and extremely disincentivized.
#[derive(PartialEq, Eq, Debug)]
pub enum Misbehavior<C: Context> {
	/// Voted invalid and valid on validity.
	ValidityDoubleVote(ValidityDoubleVote<C>),
	/// Submitted multiple candidates.
	MultipleCandidates(MultipleCandidates<C>),
	/// Submitted a message withou
	UnauthorizedStatement(UnauthorizedStatement<C>),
}

// kinds of votes for validity
#[derive(Clone, PartialEq, Eq)]
enum ValidityVote<S: Eq + Clone> {
	// implicit validity vote by issuing
	Issued(S),
	// direct validity vote
	Valid(S),
	// direct invalidity vote
	Invalid(S),
}

/// Stores votes and data about a candidate.
pub struct CandidateData<C: Context> {
	group_id: C::GroupId,
	candidate: C::Candidate,
	validity_votes: HashMap<C::ValidatorId, ValidityVote<C::Signature>>,
	availability_votes: HashMap<C::ValidatorId, C::Signature>,
	indicated_bad_by: Vec<C::ValidatorId>,
}

impl<C: Context> CandidateData<C> {
	/// whether this has been indicated bad by anyone.
	pub fn indicated_bad(&self) -> bool {
		!self.indicated_bad_by.is_empty()
	}

	/// Get an iterator over those who have indicated this candidate valid.
	// TODO: impl trait
	pub fn voted_valid_by<'a>(&'a self) -> Box<Iterator<Item=C::ValidatorId> + 'a> {
		Box::new(self.validity_votes.iter().filter_map(|(v, vote)| {
			match *vote {
				ValidityVote::Issued(_) | ValidityVote::Valid(_) => Some(v.clone()),
				ValidityVote::Invalid(_) => None,
			}
		}))
	}

	// Candidate data can be included in a proposal
	// if it has enough validity and availability votes
	// and no validators have called it bad.
	fn can_be_included(&self, validity_threshold: usize, availability_threshold: usize) -> bool {
		self.indicated_bad_by.is_empty()
			&& self.validity_votes.len() >= validity_threshold
			&& self.availability_votes.len() >= availability_threshold
	}
}

/// Create a new, empty statement table.
pub fn create<C: Context>() -> Table<C> {
	Table {
		proposed_candidates: HashMap::default(),
		detected_misbehavior: HashMap::default(),
		candidate_votes: HashMap::default(),
	}
}

/// Stores votes
#[derive(Default)]
pub struct Table<C: Context> {
	proposed_candidates: HashMap<C::ValidatorId, (C::Digest, C::Signature)>,
	detected_misbehavior: HashMap<C::ValidatorId, Misbehavior<C>>,
	candidate_votes: HashMap<C::Digest, CandidateData<C>>,
}

impl<C: Context> Table<C> {
	/// Produce a set of proposed candidates.
	///
	/// This will be at most one per group, consisting of the
	/// best candidate for each group with requisite votes for inclusion.
	pub fn proposed_candidates(&self, context: &C) -> Vec<C::Candidate> {
		use std::collections::BTreeMap;
		use std::collections::btree_map::Entry as BTreeEntry;

		let mut best_candidates = BTreeMap::new();
		for candidate_data in self.candidate_votes.values() {
			let group_id = &candidate_data.group_id;
			let (validity_t, availability_t) = context.requisite_votes(group_id);

			if !candidate_data.can_be_included(validity_t, availability_t) { continue }
			let candidate = &candidate_data.candidate;
			match best_candidates.entry(group_id.clone()) {
				BTreeEntry::Occupied(mut occ) => {
					let mut candidate_ref = occ.get_mut();
					if *candidate_ref < candidate {
						*candidate_ref = candidate;
					}
				}
				BTreeEntry::Vacant(vacant) => { vacant.insert(candidate); },
			}
		}

		best_candidates.values().map(|v| C::Candidate::clone(v)).collect::<Vec<_>>()
	}

	/// Get an iterator of all candidates with a given group.
	// TODO: impl iterator
	pub fn candidates_in_group<'a>(&'a self, group_id: C::GroupId)
		-> Box<Iterator<Item=&'a CandidateData<C>> + 'a>
	{
		Box::new(self.candidate_votes.values().filter(move |c| c.group_id == group_id))
	}

	/// Drain all misbehavior observed up to this point.
	pub fn drain_misbehavior(&mut self) -> HashMap<C::ValidatorId, Misbehavior<C>> {
		::std::mem::replace(&mut self.detected_misbehavior, HashMap::new())
	}

	/// Import a signed statement.
	pub fn import_statement(&mut self, context: &C, statement: SignedStatement<C>) {
		let signer = match context.statement_signer(&statement) {
			None => return,
			Some(signer) => signer,
		};

		let maybe_misbehavior = match statement.statement {
			Statement::Candidate(candidate) => self.import_candidate(
				context,
				signer.clone(),
				candidate,
				statement.signature
			),
			Statement::Valid(digest) => self.validity_vote(
				context,
				signer.clone(),
				digest,
				ValidityVote::Valid(statement.signature),
			),
			Statement::Invalid(digest) => self.validity_vote(
				context,
				signer.clone(),
				digest,
				ValidityVote::Invalid(statement.signature),
			),
			Statement::Available(digest) => self.availability_vote(
				context,
				signer.clone(),
				digest,
				statement.signature,
			)
		};

		if let Some(misbehavior) = maybe_misbehavior {
			// all misbehavior in agreement is provable and actively malicious.
			// punishments are not cumulative.
			self.detected_misbehavior.insert(signer, misbehavior);
		}
	}

	fn import_candidate(
		&mut self,
		context: &C,
		from: C::ValidatorId,
		candidate: C::Candidate,
		signature: C::Signature,
	) -> Option<Misbehavior<C>> {
		let group = context.candidate_group(&candidate);
		if !context.is_member_of(&from, &group) {
			return Some(Misbehavior::UnauthorizedStatement(UnauthorizedStatement {
				statement: SignedStatement {
					signature,
					statement: Statement::Candidate(candidate),
				},
			}));
		}

		// check that validator hasn't already specified another candidate.
		let digest = context.candidate_digest(&candidate);

		match self.proposed_candidates.entry(from.clone()) {
			Entry::Occupied(occ) => {
				// if digest is different, fetch candidate and
				// note misbehavior.
				let old_digest = &occ.get().0;
				if old_digest != &digest {
					let old_candidate = self.candidate_votes.get(old_digest)
						.expect("proposed digest implies existence of votes entry; qed")
						.candidate
						.clone();

					return Some(Misbehavior::MultipleCandidates(MultipleCandidates {
						first: (old_candidate, occ.get().1.clone()),
						second: (candidate, signature.clone()),
					}));
				}
			}
			Entry::Vacant(vacant) => {
				vacant.insert((digest.clone(), signature.clone()));

				// TODO: seed validity votes with issuer here?
				self.candidate_votes.entry(digest.clone()).or_insert_with(move || CandidateData {
					group_id: group,
					candidate: candidate,
					validity_votes: HashMap::new(),
					availability_votes: HashMap::new(),
					indicated_bad_by: Vec::new(),
				});
			}
		}

		self.validity_vote(
			context,
			from,
			digest,
			ValidityVote::Issued(signature),
		)
	}

	fn validity_vote(
		&mut self,
		context: &C,
		from: C::ValidatorId,
		digest: C::Digest,
		vote: ValidityVote<C::Signature>,
	) -> Option<Misbehavior<C>> {
		let votes = match self.candidate_votes.get_mut(&digest) {
			None => return None, // TODO: queue up but don't get DoS'ed
			Some(votes) => votes,
		};

		// check that this validator actually can vote in this group.
		if !context.is_member_of(&from, &votes.group_id) {
			let (sig, valid) = match vote {
				ValidityVote::Valid(s) => (s, true),
				ValidityVote::Invalid(s) => (s, false),
				ValidityVote::Issued(_) =>
					panic!("implicit issuance vote only cast if the candidate entry already created successfully; qed"),
			};

			return Some(Misbehavior::UnauthorizedStatement(UnauthorizedStatement {
				statement: SignedStatement {
					signature: sig,
					statement: if valid {
						Statement::Valid(digest)
					} else {
						Statement::Invalid(digest)
					}
				}
			}));
		}

		// check for double votes.
		match votes.validity_votes.entry(from.clone()) {
			Entry::Occupied(occ) => {
				if occ.get() != &vote {
					let double_vote_proof = match (occ.get().clone(), vote) {
						(ValidityVote::Issued(iss), ValidityVote::Valid(good)) |
						(ValidityVote::Valid(good), ValidityVote::Issued(iss)) =>
							ValidityDoubleVote::IssuedAndValidity((votes.candidate.clone(), iss), (digest, good)),
						(ValidityVote::Issued(iss), ValidityVote::Invalid(bad)) |
						(ValidityVote::Invalid(bad), ValidityVote::Issued(iss)) =>
							ValidityDoubleVote::IssuedAndInvalidity((votes.candidate.clone(), iss), (digest, bad)),
						(ValidityVote::Valid(good), ValidityVote::Invalid(bad)) |
						(ValidityVote::Invalid(bad), ValidityVote::Valid(good)) =>
							ValidityDoubleVote::ValidityAndInvalidity(digest, good, bad),
						_ => {
							// this would occur if two different but valid signatures
							// on the same kind of vote occurred.
							return None;
						}
					};

					return Some(Misbehavior::ValidityDoubleVote(double_vote_proof));
				}
			}
			Entry::Vacant(vacant) => {
				if let ValidityVote::Invalid(_) = vote {
					votes.indicated_bad_by.push(from);
				}

				vacant.insert(vote);
			}
		}

		None
	}

	fn availability_vote(
		&mut self,
		context: &C,
		from: C::ValidatorId,
		digest: C::Digest,
		signature: C::Signature,
	) -> Option<Misbehavior<C>> {
		let votes = match self.candidate_votes.get_mut(&digest) {
			None => return None, // TODO: queue up but don't get DoS'ed
			Some(votes) => votes,
		};

		// check that this validator actually can vote in this group.
		if !context.is_availability_guarantor_of(&from, &votes.group_id) {
			return Some(Misbehavior::UnauthorizedStatement(UnauthorizedStatement {
				statement: SignedStatement {
					signature: signature.clone(),
					statement: Statement::Available(digest),
				}
			}));
		}

		votes.availability_votes.insert(from, signature);
		None
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::collections::HashMap;

	#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
	struct ValidatorId(usize);

	#[derive(Debug, Copy, Clone, Hash, PartialOrd, Ord, PartialEq, Eq)]
	struct GroupId(usize);

	// group, body
	#[derive(Debug, Copy, Clone, Hash, PartialOrd, Ord, PartialEq, Eq)]
	struct Candidate(usize, usize);

	#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
	struct Signature(usize);

	#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
	struct Digest(usize);

	#[derive(Debug, PartialEq, Eq)]
	struct TestContext {
		// v -> (validity, availability)
		validators: HashMap<ValidatorId, (GroupId, GroupId)>
	}

	impl Context for TestContext {
		type ValidatorId = ValidatorId;
		type Digest = Digest;
		type Candidate = Candidate;
		type GroupId = GroupId;
		type Signature = Signature;

		fn candidate_digest(&self, candidate: &Candidate) -> Digest {
			Digest(candidate.1)
		}

		fn candidate_group(&self, candidate: &Candidate) -> GroupId {
			GroupId(candidate.0)
		}

		fn is_member_of(
			&self,
			validator: &ValidatorId,
			group: &GroupId
		) -> bool {
			self.validators.get(validator).map(|v| &v.0 == group).unwrap_or(false)
		}

		fn is_availability_guarantor_of(
			&self,
			validator: &ValidatorId,
			group: &GroupId
		) -> bool {
			self.validators.get(validator).map(|v| &v.1 == group).unwrap_or(false)
		}

		fn statement_signer(
			&self,
			statement: &SignedStatement<Self>,
		) -> Option<ValidatorId> {
			Some(ValidatorId(statement.signature.0))
		}

		fn requisite_votes(&self, _id: &GroupId) -> (usize, usize) {
			(6, 34)
		}
	}

	#[test]
	fn submitting_two_candidates_is_misbehavior() {
		let context = TestContext {
			validators: {
				let mut map = HashMap::new();
				map.insert(ValidatorId(1), (GroupId(2), GroupId(455)));
				map
			}
		};

		let mut table = create();
		let statement_a = SignedStatement {
			statement: Statement::Candidate(Candidate(2, 100)),
			signature: Signature(1),
		};

		let statement_b = SignedStatement {
			statement: Statement::Candidate(Candidate(2, 999)),
			signature: Signature(1),
		};

		table.import_statement(&context, statement_a);
		assert!(!table.detected_misbehavior.contains_key(&ValidatorId(1)));

		table.import_statement(&context, statement_b);
		assert_eq!(
			table.detected_misbehavior.get(&ValidatorId(1)).unwrap(),
			&Misbehavior::MultipleCandidates(MultipleCandidates {
				first: (Candidate(2, 100), Signature(1)),
				second: (Candidate(2, 999), Signature(1)),
			})
		);
	}

	#[test]
	fn submitting_candidate_from_wrong_group_is_misbehavior() {
		let context = TestContext {
			validators: {
				let mut map = HashMap::new();
				map.insert(ValidatorId(1), (GroupId(3), GroupId(455)));
				map
			}
		};

		let mut table = create();
		let statement = SignedStatement {
			statement: Statement::Candidate(Candidate(2, 100)),
			signature: Signature(1),
		};

		table.import_statement(&context, statement);

		assert_eq!(
			table.detected_misbehavior.get(&ValidatorId(1)).unwrap(),
			&Misbehavior::UnauthorizedStatement(UnauthorizedStatement {
				statement: SignedStatement {
					statement: Statement::Candidate(Candidate(2, 100)),
					signature: Signature(1),
				},
			})
		);
	}

	#[test]
	fn unauthorized_votes() {
		let context = TestContext {
			validators: {
				let mut map = HashMap::new();
				map.insert(ValidatorId(1), (GroupId(2), GroupId(455)));
				map.insert(ValidatorId(2), (GroupId(3), GroupId(222)));
				map
			}
		};

		let mut table = create();

		let candidate_a = SignedStatement {
			statement: Statement::Candidate(Candidate(2, 100)),
			signature: Signature(1),
		};
		let candidate_a_digest = Digest(100);

		let candidate_b = SignedStatement {
			statement: Statement::Candidate(Candidate(3, 987)),
			signature: Signature(2),
		};
		let candidate_b_digest = Digest(987);

		table.import_statement(&context, candidate_a);
		table.import_statement(&context, candidate_b);
		assert!(!table.detected_misbehavior.contains_key(&ValidatorId(1)));
		assert!(!table.detected_misbehavior.contains_key(&ValidatorId(2)));

		// validator 1 votes for availability on 2's candidate.
		let bad_availability_vote = SignedStatement {
			statement: Statement::Available(candidate_b_digest.clone()),
			signature: Signature(1),
		};
		table.import_statement(&context, bad_availability_vote);

		assert_eq!(
			table.detected_misbehavior.get(&ValidatorId(1)).unwrap(),
			&Misbehavior::UnauthorizedStatement(UnauthorizedStatement {
				statement: SignedStatement {
					statement: Statement::Available(candidate_b_digest),
					signature: Signature(1),
				},
			})
		);

		// validator 2 votes for validity on 1's candidate.
		let bad_validity_vote = SignedStatement {
			statement: Statement::Valid(candidate_a_digest.clone()),
			signature: Signature(2),
		};
		table.import_statement(&context, bad_validity_vote);

		assert_eq!(
			table.detected_misbehavior.get(&ValidatorId(2)).unwrap(),
			&Misbehavior::UnauthorizedStatement(UnauthorizedStatement {
				statement: SignedStatement {
					statement: Statement::Valid(candidate_a_digest),
					signature: Signature(2),
				},
			})
		);
	}

	#[test]
	fn validity_double_vote_is_misbehavior() {
		let context = TestContext {
			validators: {
				let mut map = HashMap::new();
				map.insert(ValidatorId(1), (GroupId(2), GroupId(455)));
				map.insert(ValidatorId(2), (GroupId(2), GroupId(246)));
				map
			}
		};

		let mut table = create();
		let statement = SignedStatement {
			statement: Statement::Candidate(Candidate(2, 100)),
			signature: Signature(1),
		};
		let candidate_digest = Digest(100);

		table.import_statement(&context, statement);
		assert!(!table.detected_misbehavior.contains_key(&ValidatorId(1)));

		let valid_statement = SignedStatement {
			statement: Statement::Valid(candidate_digest.clone()),
			signature: Signature(2),
		};

		let invalid_statement = SignedStatement {
			statement: Statement::Invalid(candidate_digest.clone()),
			signature: Signature(2),
		};

		table.import_statement(&context, valid_statement);
		assert!(!table.detected_misbehavior.contains_key(&ValidatorId(2)));

		table.import_statement(&context, invalid_statement);

		assert_eq!(
			table.detected_misbehavior.get(&ValidatorId(2)).unwrap(),
			&Misbehavior::ValidityDoubleVote(ValidityDoubleVote::ValidityAndInvalidity(
				candidate_digest,
				Signature(2),
				Signature(2),
			))
		);
	}

	#[test]
	fn issue_and_vote_is_misbehavior() {
		let context = TestContext {
			validators: {
				let mut map = HashMap::new();
				map.insert(ValidatorId(1), (GroupId(2), GroupId(455)));
				map
			}
		};

		let mut table = create();
		let statement = SignedStatement {
			statement: Statement::Candidate(Candidate(2, 100)),
			signature: Signature(1),
		};
		let candidate_digest = Digest(100);

		table.import_statement(&context, statement);
		assert!(!table.detected_misbehavior.contains_key(&ValidatorId(1)));

		let extra_vote = SignedStatement {
			statement: Statement::Valid(candidate_digest.clone()),
			signature: Signature(1),
		};

		table.import_statement(&context, extra_vote);
		assert_eq!(
			table.detected_misbehavior.get(&ValidatorId(1)).unwrap(),
			&Misbehavior::ValidityDoubleVote(ValidityDoubleVote::IssuedAndValidity(
				(Candidate(2, 100), Signature(1)),
				(Digest(100), Signature(1)),
			))
		);
	}

	#[test]
	fn candidate_can_be_included() {
		let validity_threshold = 6;
		let availability_threshold = 34;

		let mut candidate = CandidateData::<TestContext> {
			group_id: GroupId(4),
			candidate: Candidate(4, 12345),
			validity_votes: HashMap::new(),
			availability_votes: HashMap::new(),
			indicated_bad_by: Vec::new(),
		};

		assert!(!candidate.can_be_included(validity_threshold, availability_threshold));

		for i in 0..validity_threshold {
			candidate.validity_votes.insert(ValidatorId(i + 100), ValidityVote::Valid(Signature(i + 100)));
		}

		assert!(!candidate.can_be_included(validity_threshold, availability_threshold));

		for i in 0..availability_threshold {
			candidate.availability_votes.insert(ValidatorId(i + 255), Signature(i + 255));
		}

		assert!(candidate.can_be_included(validity_threshold, availability_threshold));

		candidate.indicated_bad_by.push(ValidatorId(1024));

		assert!(!candidate.can_be_included(validity_threshold, availability_threshold));
	}
}