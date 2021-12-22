//! # XYK pallet

#![cfg_attr(not(feature = "std"), no_std)]

use frame_support::{
	dispatch::{DispatchError, DispatchResult},
	ensure,
	traits::OnUnbalanced,
	weights::Pays,
	PalletId,
};
use mangata_primitives::{Balance, TokenId};

use frame_support::{
	pallet_prelude::*,
	storage::{child, generator::StorageMap as GStorageMap},
	traits::{ExistenceRequirement, OneSessionHandler, UnfilteredDispatchable, WithdrawReasons},
	weights::GetDispatchInfo,
};
use frame_system::{pallet_prelude::*, RawOrigin};
use orml_tokens::MultiTokenCurrency;
use scale_info::TypeInfo;
use sp_core::storage::ChildInfo;
use sp_runtime::{
	traits::{Hash, Zero},
	KeyTypeId, RuntimeAppPublic,
};
use sp_std::collections::btree_map::BTreeMap;

// #[cfg(test)]
// mod mock;
// #[cfg(test)]
// mod tests;

pub const XXTX_KEY_TYPE_ID: KeyTypeId = KeyTypeId(*b"xxtx");

pub mod ecdsa {
	mod app_ecdsa {
		use sp_application_crypto::{app_crypto, ecdsa};
		app_crypto!(ecdsa, super::super::XXTX_KEY_TYPE_ID);
	}

	sp_application_crypto::with_pair! {
		/// An xxtx keypair using sr25519 as its crypto.
		pub type AuthorityPair = app_ecdsa::Pair;
	}

	/// An xxtx signature using sr25519 as its crypto.
	pub type AuthoritySignature = app_ecdsa::Signature;

	/// An xxtx identifier using sr25519 as its crypto.
	pub type AuthorityId = app_ecdsa::Public;
}

// syntactic sugar for logging.
#[macro_export]
macro_rules! log {
	($level:tt, $patter:expr $(, $values:expr)* $(,)?) => {
		log::$level!(
			target: crate::LOG_TARGET,
			concat!("[{:?}] 💸 ", $patter), <frame_system::Pallet<T>>::block_number() $(, $values)*
		)
	};
}

const PALLET_ID: PalletId = PalletId(*b"79b14c96");
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo)]
pub struct TxnRegistryDetails<AccountId, Index> {
	pub doubly_encrypted_call: Vec<u8>,
	pub user: AccountId,
	pub nonce: Index,
	pub weight: Weight,
	pub builder: AccountId,
	pub executor: AccountId,
	pub singly_encrypted_call: Option<Vec<u8>>,
	pub decrypted_call: Option<Vec<u8>>,
}

pub use pallet::*;

#[frame_support::pallet]
pub mod pallet {

	use super::*;

	#[pallet::pallet]
	#[pallet::generate_store(pub(super) trait Store)]
	pub struct Pallet<T>(PhantomData<T>);

	#[pallet::config]
	pub trait Config: frame_system::Config + pallet_session::Config {
		type Event: From<Event<Self>> + IsType<<Self as frame_system::Config>::Event>;
		type Tokens: MultiTokenCurrency<Self::AccountId>;
		type AuthorityId: Member + Parameter + RuntimeAppPublic + Default + Ord;
		type Fee: Get<Balance>;
		type Treasury: OnUnbalanced<
			<Self::Tokens as MultiTokenCurrency<Self::AccountId>>::NegativeImbalance,
		>;
		type Call: Parameter
			+ UnfilteredDispatchable<Origin = Self::Origin>
			+ GetDispatchInfo
			+ From<Call<Self>>;
		type DoublyEncryptedCallMaxLength: Get<u32>;
	}

	#[pallet::error]
	/// Errors
	pub enum Error<T> {
		IncorrectCallWeight,
		NoMarkedRefund,
		CallDeserilizationFailed,
		DoublyEncryptedCallMaxLengthExceeded,
		TxnDoesNotExistsInRegistry,
		UnexpectedError,
	}

	#[pallet::event]
	#[pallet::generate_deposit(pub(super) fn deposit_event)]
	pub enum Event<T: Config> {
		//TODO add trading events
		Called(DispatchResult, T::AccountId, T::Index, T::Hash),

		/// Calls were executed
		CallsExecuted(T::AccountId, T::Index, T::Hash),

		/// A user has submitted a doubly encrypted transaction.
		DoublyEncryptedTxnSubmitted(T::AccountId, T::Index, T::Hash),

		/// A collator has submitted a singly encrypted transaction.
		SinglyEncryptedTxnSubmitted(T::AccountId, T::Index, T::Hash),

		/// A collator has submitted a decrypted transaction.
		DecryptedTransactionSubmitted(T::AccountId, T::Index, T::Hash),

		/// User refunded
		UserRefunded(T::Index, T::AccountId, T::Index, T::Hash, Balance),
	}

	#[pallet::storage]
	#[pallet::getter(fn keys)]
	pub type KeyMap<T: Config> =
		StorageValue<_, BTreeMap<T::AccountId, T::AuthorityId>, ValueQuery>;

	#[pallet::storage]
	#[pallet::getter(fn txn_registry)]
	pub type TxnRegistry<T: Config> = StorageMap<
		_,
		Blake2_128Concat,
		T::Hash,
		Option<TxnRegistryDetails<T::AccountId, T::Index>>,
		ValueQuery,
	>;

	#[pallet::storage]
	#[pallet::getter(fn doubly_encrypted_queue)]
	pub type DoublyEncryptedQueue<T: Config> =
		StorageMap<_, Blake2_128Concat, T::AccountId, Vec<T::Hash>, ValueQuery>;

	#[pallet::storage]
	#[pallet::getter(fn singly_encrypted_queue)]
	pub type SinglyEncryptedQueue<T: Config> =
		StorageMap<_, Blake2_128Concat, T::AccountId, Vec<T::Hash>, ValueQuery>;

	#[pallet::storage]
	#[pallet::getter(fn txn_record)]
	pub type TxnRecord<T: Config> = StorageDoubleMap<
		_,
		Blake2_128Concat,
		T::Index,
		Blake2_128Concat,
		T::AccountId,
		BTreeMap<T::Hash, (T::Index, Balance, bool)>,
		ValueQuery,
	>;

	#[pallet::storage]
	#[pallet::getter(fn execd_txn_record)]
	pub type ExecutedTxnRecord<T: Config> = StorageDoubleMap<
		_,
		Blake2_128Concat,
		T::Index,
		Blake2_128Concat,
		T::AccountId,
		Vec<T::Hash>,
		ValueQuery,
	>;

	// XYK extrinsics.
	#[pallet::call]
	impl<T: Config> Pallet<T> {
		#[pallet::weight(10_000)]
		pub fn submit_doubly_encrypted_transaction(
			origin: OriginFor<T>,
			doubly_encrypted_call: Vec<u8>,
			nonce: T::Index,
			weight: Weight,
			builder: T::AccountId,
			executor: T::AccountId,
		) -> DispatchResult {
			let user = ensure_signed(origin)?;

			ensure!(
				doubly_encrypted_call.len() <= T::DoublyEncryptedCallMaxLength::get() as usize,
				Error::<T>::DoublyEncryptedCallMaxLengthExceeded
			);
			//
			let fee_charged = T::Fee::get();

			T::Tokens::ensure_can_withdraw(
				0u8.into(),
				&user,
				fee_charged.into(),
				WithdrawReasons::all(),
				Default::default(),
			)?;
			let negative_imbalance = T::Tokens::withdraw(
				0u8.into(),
				&user,
				fee_charged.into(),
				WithdrawReasons::all(),
				ExistenceRequirement::AllowDeath,
			)?;
			T::Treasury::on_unbalanced(negative_imbalance);

			let mut identifier_vec: Vec<u8> = Vec::<u8>::new();
			identifier_vec.extend_from_slice(&doubly_encrypted_call[..]);
			identifier_vec.extend_from_slice(&Encode::encode(&user)[..]);
			identifier_vec.extend_from_slice(&Encode::encode(&nonce)[..]);

			let identifier: T::Hash = T::Hashing::hash_of(&identifier_vec);

			let txn_registry_details = TxnRegistryDetails {
				doubly_encrypted_call,
				user: user.clone(),
				nonce,
				weight,
				builder: builder.clone(),
				executor,
				singly_encrypted_call: None,
				decrypted_call: None,
			};

			TxnRegistry::<T>::insert(identifier, Some(txn_registry_details));
			DoublyEncryptedQueue::<T>::mutate(&builder, |vec_hash| vec_hash.push(identifier));
			TxnRecord::<T>::mutate(
				(T::Index::from(<pallet_session::Pallet<T>>::current_index()), &user),
				|tree_record| tree_record.insert(identifier, (nonce, fee_charged, false)),
			);
			Self::deposit_event(Event::DoublyEncryptedTxnSubmitted(user, nonce, identifier));
			Ok(())
		}

		#[pallet::weight(10_000)]
		pub fn submit_singly_encrypted_transaction(
			origin: OriginFor<T>,
			identifier: T::Hash,
			singly_encrypted_call: Vec<u8>,
		) -> DispatchResult {
			ensure_none(origin)?;
			TxnRegistry::<T>::try_mutate(
				identifier,
				|txn_registry_details_option| -> DispatchResult {
					if let Some(ref mut txn_registry_details) = txn_registry_details_option {
						DoublyEncryptedQueue::<T>::mutate(
							&txn_registry_details.builder,
							|vec_hash| vec_hash.retain(|x| *x != identifier),
						);
						SinglyEncryptedQueue::<T>::mutate(
							&txn_registry_details.executor,
							|vec_hash| vec_hash.push(identifier),
						);
						txn_registry_details.singly_encrypted_call = Some(singly_encrypted_call);

						Self::deposit_event(Event::SinglyEncryptedTxnSubmitted(
							txn_registry_details.user.clone(),
							txn_registry_details.nonce,
							identifier,
						));

						Ok(())
					} else {
						Err(DispatchError::from(Error::<T>::TxnDoesNotExistsInRegistry))
					}
				},
			)
		}

		#[pallet::weight(10_000)]
		// //TODO: make use of _weight parameter, collator should precalculate weight of decrypted
		// //transactions
		pub fn submit_decrypted_transaction(
			origin: OriginFor<T>,
			identifier: T::Hash,
			decrypted_call: Vec<u8>,
			_weight: Weight,
		) -> DispatchResult {
			ensure_none(origin)?;

			let mut txn_registry_details = TxnRegistry::<T>::get(identifier)
				.ok_or_else(|| Error::<T>::TxnDoesNotExistsInRegistry)?;
			SinglyEncryptedQueue::<T>::mutate(&txn_registry_details.executor, |vec_hash| {
				vec_hash.retain(|x| *x != identifier)
			});

			ExecutedTxnRecord::<T>::mutate(
				(
					T::Index::from(<pallet_session::Pallet<T>>::current_index()),
					&txn_registry_details.user,
				),
				|vec_hash| vec_hash.push(identifier),
			);

			txn_registry_details.decrypted_call = Some(decrypted_call.clone());

			TxnRegistry::<T>::insert(identifier, Some(txn_registry_details.clone()));

			Self::deposit_event(Event::DecryptedTransactionSubmitted(
				txn_registry_details.user.clone(),
				txn_registry_details.nonce,
				identifier,
			));

			let calls: Vec<Box<<T as Config>::Call>> = Decode::decode(&mut &decrypted_call[..])
				.map_err(|_| DispatchError::from(Error::<T>::CallDeserilizationFailed))?;

			Pallet::<T>::execute_calls(
				RawOrigin::Root.into(),
				calls,
				txn_registry_details.user,
				identifier,
				txn_registry_details.nonce,
				txn_registry_details.weight,
			)?;

			Ok(())
		}

		#[pallet::weight(10_000)]
		// #[weight = (weight.saturating_add(10_000), Pays::No)]
		pub fn execute_calls(
			origin: OriginFor<T>,
			calls: Vec<Box<<T as Config>::Call>>,
			user_account: T::AccountId,
			identifier: T::Hash,
			nonce: T::Index,
			weight: Weight,
		) -> DispatchResult {
			ensure_root(origin)?;

			let mut calls_weight: u128 = u128::zero();
			for call in calls.iter() {
				calls_weight = calls_weight.saturating_add(call.get_dispatch_info().weight.into());
			}
			if calls_weight > weight.into() {
				return Err(DispatchError::from(Error::<T>::IncorrectCallWeight))
			}

			for call in calls {
				let res = call.dispatch_bypass_filter(
					frame_system::RawOrigin::Signed(user_account.clone()).into(),
				);
				Self::deposit_event(Event::Called(
					res.map(|_| ()).map_err(|e| e.error),
					user_account.clone(),
					nonce,
					identifier,
				));
			}

			Self::deposit_event(Event::CallsExecuted(user_account, nonce, identifier));

			Ok(())
		}
		//

		#[pallet::weight(10_000)]
		pub fn refund_user(origin: OriginFor<T>, identifier: T::Hash) -> DispatchResult {
			let user = ensure_signed(origin)?;
			let current_session_index = <pallet_session::Pallet<T>>::current_index();
			let previous_session_index: T::Index = current_session_index
				.checked_sub(1u8.into())
				.ok_or_else(|| DispatchError::from(Error::<T>::NoMarkedRefund))?
				.into();

			if ExecutedTxnRecord::<T>::get((previous_session_index, &user)).contains(&identifier) {
				return Err(DispatchError::from(Error::<T>::NoMarkedRefund))
			} else {
				let (nonce, fee_charged, already_refunded) =
					TxnRecord::<T>::get((previous_session_index, &user))
						.get(&identifier)
						.ok_or_else(|| DispatchError::from(Error::<T>::NoMarkedRefund))?
						.clone();

				ensure!(!already_refunded, Error::<T>::NoMarkedRefund);

				// TODO
				// Refund fee
				TxnRecord::<T>::mutate(
					(T::Index::from(<pallet_session::Pallet<T>>::current_index()), &user),
					|tree_record| tree_record.insert(identifier, (nonce, fee_charged, true)),
				);

				Self::deposit_event(Event::UserRefunded(
					previous_session_index,
					user,
					nonce,
					identifier,
					fee_charged,
				));
			}

			Ok(())
		}
	}
}

impl<T: Config> sp_runtime::BoundToRuntimeAppPublic for Pallet<T> {
	type Public = T::AuthorityId;
}

impl<T: Config> OneSessionHandler<T::AccountId> for Pallet<T> {
	type Key = T::AuthorityId;

	fn on_genesis_session<'a, I: 'a>(validators: I)
	where
		I: Iterator<Item = (&'a T::AccountId, T::AuthorityId)>,
	{
		let keys = validators.map(|(x, y)| (x.clone(), y)).collect::<BTreeMap<_, _>>();
		Self::initialize_keys(&keys);
	}

	fn on_new_session<'a, I: 'a>(_changed: bool, validators: I, _queued_validators: I)
	where
		I: Iterator<Item = (&'a T::AccountId, T::AuthorityId)>,
	{
		// Remember who the authorities are for the new session.
		KeyMap::<T>::put(validators.collect::<BTreeMap<_, _>>());
	}

	fn on_before_session_ending() {
		KeyMap::<T>::kill();

		child::kill_storage(
			&ChildInfo::new_default_from_vec(TxnRegistry::<T>::prefix_hash()),
			None,
		);
		child::kill_storage(
			&ChildInfo::new_default_from_vec(DoublyEncryptedQueue::<T>::prefix_hash()),
			None,
		);
		child::kill_storage(
			&ChildInfo::new_default_from_vec(SinglyEncryptedQueue::<T>::prefix_hash()),
			None,
		);

		let session_index = <pallet_session::Pallet<T>>::current_index();

		if let Some(previous_session_index) = session_index.checked_sub(1u8.into()) {
			TxnRecord::<T>::remove_prefix(T::Index::from(previous_session_index));
			ExecutedTxnRecord::<T>::remove_prefix(T::Index::from(previous_session_index));
		}
	}

	fn on_disabled(_i: u32) {
		// ignore
	}
}
