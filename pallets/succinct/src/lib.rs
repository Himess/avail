#![cfg_attr(not(feature = "std"), no_std)]

use frame_support::traits::{Currency, ExistenceRequirement, UnixTime};
use frame_support::{pallet_prelude::*, parameter_types, PalletId};
use hex_literal::hex;
use sp_core::H256;
use sp_runtime::SaturatedConversion;

pub use pallet::*;

use crate::verifier::Verifier;

#[cfg(feature = "runtime-benchmarks")]
mod benchmarking;
#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;
// mod verify;
mod state;
mod target_amb;
mod verifier;
mod weights;

type VerificationKeyDef<T> = BoundedVec<u8, <T as Config>::MaxVerificationKeyLength>;

parameter_types! {
	pub const StepFunctionId: H256 = H256(hex!("af44af6890508b3b7f6910d4a4570a0d524769a23ce340b2c7400e140ad168ab"));
	pub const RotateFunctionId: H256 = H256(hex!("9aed23f9e6e8f8b98751cf508069b5b7f015d4d510b6a4820d41ba1ce88190d9"));

	// Constants
	pub const MinSyncCommitteeParticipants: u16=10;
	pub const SyncCommitteeSize: u32=512;
	pub const FinalizedRootIndex: u32=105;
	pub const NextSyncCommitteeIndex: u32= 55;
	pub const ExecutionStateRootIndex: u32= 402;
	pub const MaxPublicInputsLength: u32 = 9;
	pub const MaxVerificationKeyLength: u32 = 4143;
	pub const MaxProofLength: u32 = 1133;

	pub const MessageVersion: u8 = 1;
	pub const MinLightClientDelay: u64 = 120;
	pub const MessageMappingStorageIndex:u64 = 1;


	// TODO bounded vec size
	pub const InputMaxLen: u32 = 256;
	pub const OutputMaxLen: u32 = 512;
	pub const ProofMaxLen: u32 = 2048;

	pub const MessageBytesMaxLen: u32 = 2048;
	pub const AccountProofMaxLen: u32 = 2048;
	pub const AccountProofLen: u32 = 2048;
	pub const StorageProofMaxLen: u32 = 2048;
	pub const StorageProofLen: u32 = 2048;


	pub const BridgePalletId: PalletId = PalletId(*b"avl/brdg");

}

#[frame_support::pallet]
pub mod pallet {
	use ark_std::string::String;
	use ark_std::{vec, vec::Vec};
	use ethabi::Token;
	use ethabi::Token::Uint;
	use frame_support::dispatch::{GetDispatchInfo, UnfilteredDispatchable};
	use frame_support::traits::{Hash, LockableCurrency};
	use frame_support::{pallet_prelude::ValueQuery, DefaultNoBound};
	use frame_system::pallet_prelude::*;
	use primitive_types::H160;
	use primitive_types::{H256, U256};
	use sp_io::hashing::keccak_256;
	use sp_io::hashing::sha2_256;
	use sp_runtime::traits::AccountIdConversion;
	pub use weights::WeightInfo;

	use crate::state::State;
	use crate::state::{
		parse_rotate_output, parse_step_output, VerifiedRotateCallStore, VerifiedStepCallStore,
		VerifiedStepOutput,
	};
	use crate::target_amb::{
		decode_message, decode_message_data, get_storage_root, get_storage_value, Message,
		MessageData,
	};
	use crate::verifier::encode_packed;

	use super::*;

	#[pallet::error]
	pub enum Error<T> {
		UpdaterMisMatch,
		VerificationError,
		NotEnoughParticipants,
		TooLongVerificationKey,
		VerificationKeyIsNotSet,
		MalformedVerificationKey,
		NotSupportedCurve,
		NotSupportedProtocol,
		StepVerificationError,
		RotateVerificationError,
		HeaderRootNotSet,
		VerificationFailed,
		FunctionIdNotRecognised,
		HeaderRootAlreadySet,
		StateRootAlreadySet,
		SyncCommitteeAlreadySet,
		ProofCreationError,
		InvalidRotateProof,
		InvalidStepProof,
		//     Message execution
		MessageAlreadyExecuted,
		WrongChain,
		WrongVersion,
		BroadcasterSourceChainNotSet,
		LightClientInconsistent,
		LightClientNotSet,
		SourceChainFrozen,
		TimestampNotSet,
		MustWaitLongerForSlot,
		CannotDecodeRlpItems,
		AccountNotFound,
		CannotGetStorageRoot,
		CannotGetStorageValue,
		TrieError,
		StorageValueNotFount,
		StorageRootNotFount,
		InvalidMessageHash,
		CannotDecodeMessageData,
		CannotDecodeDestinationAccountId,
	}

	#[pallet::event]
	#[pallet::generate_deposit(pub (super) fn deposit_event)]
	pub enum Event<T: Config> {
		// emit event once the head is updated
		HeaderUpdate {
			slot: u64,
			finalization_root: H256,
		},
		// emit event once the sync committee updates
		SyncCommitteeUpdate {
			period: u64,
			root: U256,
		},
		// emit event when verification setup is completed
		VerificationSetupCompleted,
		// emit event if verification is success
		VerificationSuccess {
			who: H256,
			attested_slot: u64,
			finalized_slot: u64,
		},
		// emit when new updater is set
		NewUpdater {
			old: H256,
			new: H256,
		},
		ExecutedMessage {
			chain_id: u32,
			nonce: u64,
			message_root: H256,
			status: bool,
		},
		// emit if source chain gets frozen
		SourceChainFrozen {
			source_chain_id: u32,
			frozen: bool,
		},
	}

	// The latest slot the light client has a finalized header for.
	#[derive(
		Clone, Copy, Default, Encode, Decode, Debug, PartialEq, Eq, TypeInfo, MaxEncodedLen,
	)]
	pub enum MessageStatusEnum {
		#[default]
		NotExecuted,
		ExecutionFailed,
		ExecutionSucceeded,
	}

	#[pallet::storage]
	pub type StepVerificationKeyStorage<T: Config> =
		StorageValue<_, VerificationKeyDef<T>, ValueQuery>;

	#[pallet::storage]
	pub type RotateVerificationKeyStorage<T: Config> =
		StorageValue<_, VerificationKeyDef<T>, ValueQuery>;

	// Storage for a general state.
	#[pallet::storage]
	pub type Head<T: Config> = StorageValue<_, u64, ValueQuery>;

	// Maps from a slot to a block header root.
	#[pallet::storage]
	pub type Headers<T> = StorageMap<_, Identity, u64, H256, ValueQuery>;

	// Maps slot to the timestamp of when the headers mapping was updated with slot as a key
	#[pallet::storage]
	pub type Timestamps<T> = StorageMap<_, Identity, u64, u64, ValueQuery>;

	// Maps from a slot to the current finalized ethereum execution state root.
	#[pallet::storage]
	pub type ExecutionStateRoots<T> = StorageMap<_, Identity, u64, H256, ValueQuery>;

	// Maps from a period to the poseidon commitment for the sync committee.
	#[pallet::storage]
	pub type SyncCommitteePoseidons<T> = StorageMap<_, Identity, u64, U256, ValueQuery>;

	// Storage for a general state.
	#[pallet::storage]
	pub type StateStorage<T: Config> = StorageValue<_, State, ValueQuery>;

	#[pallet::storage]
	pub type VerifiedStepCall<T> = StorageValue<_, VerifiedStepCallStore, ValueQuery>;

	#[pallet::storage]
	pub type VerifiedRotateCall<T> = StorageValue<_, VerifiedRotateCallStore, ValueQuery>;

	#[pallet::storage]
	#[pallet::getter(fn get_message_status)]
	pub type MessageStatus<T> = StorageMap<_, Identity, H256, MessageStatusEnum, ValueQuery>;

	// Mapping between source chainId and the address of the Telepathy broadcaster on that chain.
	#[pallet::storage]
	#[pallet::getter(fn get_broadcaster)]
	pub type Broadcasters<T> = StorageMap<_, Identity, u32, H160, ValueQuery>;

	// Ability to froze source, must support possibility to update value
	#[pallet::storage]
	#[pallet::getter(fn is_frozen)]
	pub type SourceChainFrozen<T> = StorageMap<_, Identity, u32, bool, ValueQuery>;

	#[pallet::config]
	pub trait Config: frame_system::Config {
		type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;

		type Currency: LockableCurrency<Self::AccountId, Moment = BlockNumberFor<Self>>;

		type TimeProvider: UnixTime;
		#[pallet::constant]
		type MaxPublicInputsLength: Get<u32>;
		// 9
		#[pallet::constant]
		type MaxProofLength: Get<u32>;
		// 1133
		#[pallet::constant]
		type MaxVerificationKeyLength: Get<u32>;
		// 4143
		#[pallet::constant]
		type MinSyncCommitteeParticipants: Get<u32>;
		#[pallet::constant]
		type SyncCommitteeSize: Get<u32>;
		#[pallet::constant]
		type FinalizedRootIndex: Get<u32>;
		#[pallet::constant]
		type NextSyncCommitteeIndex: Get<u32>;
		#[pallet::constant]
		type ExecutionStateRootIndex: Get<u32>;

		#[pallet::constant]
		type StepFunctionId: Get<H256>;

		#[pallet::constant]
		type RotateFunctionId: Get<H256>;

		#[pallet::constant]
		type MessageVersion: Get<u8>;

		#[pallet::constant]
		type MinLightClientDelay: Get<u64>;

		#[pallet::constant]
		type MessageMappingStorageIndex: Get<u64>;

		/// Bridge's pallet id, used for deriving its sovereign account ID.
		#[pallet::constant]
		type PalletId: Get<PalletId>;

		type RuntimeCall: Parameter
			+ UnfilteredDispatchable<RuntimeOrigin = Self::RuntimeOrigin>
			+ GetDispatchInfo;

		type WeightInfo: WeightInfo;
	}

	//  pallet initialization data
	// TODO check if genesis is a good place for this
	#[pallet::genesis_config]
	#[derive(DefaultNoBound)]
	pub struct GenesisConfig<T: Config> {
		pub updater: Hash,
		pub slots_per_period: u64,
		pub source_chain_id: u32,
		pub finality_threshold: u16,
		pub sync_committee_poseidon: U256,
		pub period: u64,
		pub _phantom: PhantomData<T>,
	}

	#[pallet::genesis_build]
	impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
		// TODO init state
		fn build(&self) {
			// TODO time cannot be called at Genesis
			// T::TimeProvider::now().as_secs()
			// Preconfigure init data
			<StateStorage<T>>::put(State {
				updater: self.updater,
				slots_per_period: self.slots_per_period,
				source_chain_id: self.source_chain_id,
				finality_threshold: self.finality_threshold,
			});

			Head::<T>::set(0);
			<SyncCommitteePoseidons<T>>::insert(self.period, self.sync_committee_poseidon);

			// TODO TEST
			ExecutionStateRoots::<T>::set(
				8581263,
				H256(hex!(
					"cd187a0c3dddad24f1bb44211849cc55b6d2ff2713be85f727e9ab8c491c621c"
				)),
			);
			Broadcasters::<T>::set(5, H160(hex!("43f0222552e8114ad8f224dea89976d3bf41659d")));
		}
	}

	#[pallet::pallet]
	pub struct Pallet<T>(_);

	#[pallet::call]
	impl<T: Config> Pallet<T>
	where
		[u8; 32]: From<T::AccountId>,
	{
		#[pallet::call_index(0)]
		#[pallet::weight(T::WeightInfo::step())]
		pub fn fulfill_call(
			origin: OriginFor<T>,
			function_id: H256,
			input: BoundedVec<u8, InputMaxLen>,
			output: BoundedVec<u8, OutputMaxLen>,
			proof: BoundedVec<u8, ProofMaxLen>,
			slot: u64,
		) -> DispatchResult {
			let sender: [u8; 32] = ensure_signed(origin)?.into();
			let state = StateStorage::<T>::get();
			// ensure sender is preconfigured
			ensure!(H256(sender) == state.updater, Error::<T>::UpdaterMisMatch);
			let input_hash = H256(sha2_256(input.as_slice()));
			let output_hash = H256(sha2_256(output.as_slice()));
			let verifier = Self::get_verifier(function_id)?;

			let success = verifier
				.verify(input_hash, output_hash, proof.to_vec())
				.map_err(|_| Error::<T>::VerificationError)?;

			ensure!(success, Error::<T>::VerificationFailed);

			if function_id == StepFunctionId::get() {
				let vs = VerifiedStepCallStore::new(
					function_id,
					input_hash,
					parse_step_output(output.to_vec()),
				);
				VerifiedStepCall::<T>::set(vs);
				if Self::step_into(slot, state)? {
					Self::deposit_event(Event::HeaderUpdate {
						slot,
						finalization_root: vs.verified_output.finalized_header_root,
					});
				}
			} else if function_id == RotateFunctionId::get() {
				let vr = VerifiedRotateCallStore::new(
					function_id,
					input_hash,
					parse_rotate_output(output.to_vec()),
				);

				VerifiedRotateCall::<T>::set(vr);
				if Self::rotate_into(slot, state)? {
					Self::deposit_event(Event::SyncCommitteeUpdate {
						period: slot,
						root: vr.sync_committee_poseidon,
					});
				}
			} else {
				return Err(Error::<T>::FunctionIdNotRecognised.into());
			}

			Ok(())
		}

		/// Sets updater that can call step and rotate functions
		#[pallet::call_index(1)]
		#[pallet::weight(T::WeightInfo::step())]
		pub fn set_updater(origin: OriginFor<T>, updater: H256) -> DispatchResult {
			ensure_root(origin)?;
			let old = StateStorage::<T>::get();
			StateStorage::<T>::try_mutate(|cfg| -> Result<(), DispatchError> {
				cfg.updater = updater;
				Ok(())
			})?;

			Self::deposit_event(Event::<T>::NewUpdater {
				old: old.updater,
				new: updater,
			});
			Ok(())
		}

		/// Sets verification public inputs for step function.
		#[pallet::call_index(2)]
		#[pallet::weight(T::WeightInfo::step())]
		pub fn setup_step_verification(
			origin: OriginFor<T>,
			verification: String,
		) -> DispatchResult {
			ensure_root(origin)?;
			// try from json to Verifier struct
			Verifier::from_json_u8_slice(verification.as_bytes())
				.map_err(|_| Error::<T>::MalformedVerificationKey)?;
			// store verification to storage
			Self::store_step_verification_key(verification.as_bytes().to_vec())?;

			Self::deposit_event(Event::<T>::VerificationSetupCompleted);
			Ok(())
		}

		/// Sets verification public inputs for rotate function.
		#[pallet::call_index(3)]
		#[pallet::weight(T::WeightInfo::step())]
		pub fn setup_rotate_verification(
			origin: OriginFor<T>,
			verification: String,
		) -> DispatchResult {
			ensure_root(origin)?;
			// try from json to Verifier struct
			Verifier::from_json_u8_slice(verification.as_bytes())
				.map_err(|_| Error::<T>::MalformedVerificationKey)?;
			// store verification to storage
			Self::store_rotate_verification_key(verification.as_bytes().to_vec())?;

			Self::deposit_event(Event::<T>::VerificationSetupCompleted);
			Ok(())
		}

		#[pallet::call_index(5)]
		#[pallet::weight(T::WeightInfo::step())]
		pub fn execute(
			origin: OriginFor<T>,
			slot: u64,
			message_bytes: BoundedVec<u8, MessageBytesMaxLen>,
			account_proof: BoundedVec<BoundedVec<u8, AccountProofMaxLen>, AccountProofLen>,
			storage_proof: BoundedVec<BoundedVec<u8, StorageProofMaxLen>, StorageProofLen>,
		) -> DispatchResult {
			ensure_signed(origin)?;

			let message_root = H256(keccak_256(message_bytes.as_slice()));

			let message = decode_message(message_bytes.to_vec());
			check_preconditions::<T>(&message, message_root)?;

			ensure!(
				SourceChainFrozen::<T>::get(message.source_chain_id) == false,
				Error::<T>::SourceChainFrozen
			);

			let root = ExecutionStateRoots::<T>::get(slot);
			let broadcaster = Broadcasters::<T>::get(message.source_chain_id);

			let account_proof_vec = account_proof
				.iter()
				.map(|inner_bounded_vec| inner_bounded_vec.iter().copied().collect())
				.collect();

			let storage_root = get_storage_root(account_proof_vec, broadcaster, root)
				.map_err(|_| Error::<T>::CannotGetStorageRoot)?;

			let nonce = Uint(U256::from(message.nonce));
			let mm_idx = Uint(U256::from(MessageMappingStorageIndex::get()));
			let slot_key = H256(keccak_256(ethabi::encode(&[nonce, mm_idx]).as_slice()));

			let storage_proof_vec = storage_proof
				.iter()
				.map(|inner_bounded_vec| inner_bounded_vec.iter().copied().collect())
				.collect();

			let slot_value = get_storage_value(slot_key, storage_root, storage_proof_vec)
				.map_err(|_| Error::<T>::CannotGetStorageValue)?;

			ensure!(slot_value == message_root, Error::<T>::InvalidMessageHash);

			// TODO decode message formap
			let message_data = MessageData {
				recipient_address: H256([1u8; 32]),
				amount: U256::zero(),
			}; //Self::validate_transfer(message.data)?;

			let success = Self::transfer(message_data.amount, message_data.recipient_address)?;
			if success {
				MessageStatus::<T>::set(message_root, MessageStatusEnum::ExecutionSucceeded);
				Self::deposit_event(Event::<T>::ExecutedMessage {
					chain_id: message.source_chain_id,
					nonce: message.nonce,
					message_root,
					status: true,
				});
			} else {
				MessageStatus::<T>::set(message_root, MessageStatusEnum::ExecutionFailed);
				Self::deposit_event(Event::<T>::ExecutedMessage {
					chain_id: message.source_chain_id,
					nonce: message.nonce,
					message_root,
					status: false,
				});
			}

			Ok(())
		}

		#[pallet::call_index(6)]
		#[pallet::weight(T::WeightInfo::step())]
		pub fn source_chain_froze(
			origin: OriginFor<T>,
			source_chain_id: u32,
			frozen: bool,
		) -> DispatchResult {
			ensure_root(origin)?;

			SourceChainFrozen::<T>::set(source_chain_id, frozen);
			Self::deposit_event(Event::<T>::SourceChainFrozen {
				source_chain_id,
				frozen,
			});

			Ok(())
		}
	}

	pub fn check_preconditions<T: Config>(
		message: &Message,
		message_root: H256,
	) -> Result<(), DispatchError> {
		let message_status = MessageStatus::<T>::get(message_root);
		// Message must not be executed
		ensure!(
			message_status == MessageStatusEnum::NotExecuted,
			Error::<T>::MessageAlreadyExecuted
		);

		ensure!(
			message.version == MessageVersion::get(),
			Error::<T>::WrongVersion
		);

		let source_chain = Broadcasters::<T>::get(message.source_chain_id);
		ensure!(
			source_chain != H160::zero(),
			Error::<T>::BroadcasterSourceChainNotSet
		);

		Ok(())
	}

	impl<T: Config> Pallet<T> {
		/// The account ID of the bridge's pot.
		pub fn account_id() -> T::AccountId {
			T::PalletId::get().into_account_truncating()
		}

		pub fn transfer(amount: U256, destination_account: H256) -> Result<bool, DispatchError> {
			let destination_account_id =
				T::AccountId::decode(&mut &destination_account.encode()[..])
					.map_err(|_| Error::<T>::CannotDecodeDestinationAccountId)?;

			let transferable_amount = amount.as_u128().saturated_into();
			T::Currency::transfer(
				&Self::account_id(),
				&destination_account_id,
				transferable_amount,
				ExistenceRequirement::KeepAlive,
			)?;

			Ok(true)
		}

		pub fn validate_transfer(message_data: Vec<u8>) -> Result<MessageData, DispatchError> {
			let message_data = decode_message_data(message_data)
				.map_err(|_| Error::<T>::CannotDecodeMessageData)?;

			// TODO add some validation if needed?

			Ok(message_data)
		}

		fn rotate_into(finalized_slot: u64, state: State) -> Result<bool, DispatchError> {
			let finalized_header_root = Headers::<T>::get(finalized_slot);
			ensure!(
				finalized_header_root != H256::zero(),
				Error::<T>::HeaderRootNotSet
			);

			let input = ethabi::encode(&[Token::FixedBytes(finalized_header_root.0.to_vec())]);
			let sync_committee_poseidon: U256 =
				Self::verified_rotate_call(RotateFunctionId::get(), input)?;

			let current_period = finalized_slot / state.slots_per_period;
			let next_period = current_period + 1;

			let is_set = Self::set_sync_committee_poseidon(next_period, sync_committee_poseidon)?;

			Ok(is_set)
		}

		fn step_into(attested_slot: u64, state: State) -> Result<bool, DispatchError> {
			let current_period = attested_slot / state.slots_per_period;
			let sc_poseidon = SyncCommitteePoseidons::<T>::get(current_period);

			let input = encode_packed(sc_poseidon, attested_slot);
			let result = Self::verified_step_call(StepFunctionId::get(), input)?;

			ensure!(
				result.participation >= state.finality_threshold,
				Error::<T>::NotEnoughParticipants
			);

			let updated = Self::set_slot_roots(result)?;

			Ok(updated)
		}

		fn set_slot_roots(step_output: VerifiedStepOutput) -> Result<bool, DispatchError> {
			let header = Headers::<T>::get(step_output.finalized_slot);

			ensure!(header == H256::zero(), Error::<T>::HeaderRootAlreadySet);

			let state_root = ExecutionStateRoots::<T>::get(step_output.finalized_slot);

			ensure!(state_root == H256::zero(), Error::<T>::StateRootAlreadySet);

			Head::<T>::set(step_output.finalized_slot);

			Headers::<T>::insert(
				step_output.finalized_slot,
				step_output.finalized_header_root,
			);

			ExecutionStateRoots::<T>::insert(
				step_output.finalized_slot,
				step_output.execution_state_root,
			);

			Timestamps::<T>::insert(step_output.finalized_slot, T::TimeProvider::now().as_secs());

			Ok(true)
		}

		fn set_sync_committee_poseidon(period: u64, poseidon: U256) -> Result<bool, DispatchError> {
			let sync_committee_poseidons = SyncCommitteePoseidons::<T>::get(period);

			ensure!(
				sync_committee_poseidons == U256::zero(),
				Error::<T>::SyncCommitteeAlreadySet
			);

			SyncCommitteePoseidons::<T>::set(period, poseidon);

			Ok(true)
		}

		fn get_verifier(function_id: H256) -> Result<Verifier, Error<T>> {
			if function_id == StepFunctionId::get() {
				Self::get_step_verifier()
			} else {
				Self::get_rotate_verifier()
			}
		}

		fn get_step_verifier() -> Result<Verifier, Error<T>> {
			let vk = StepVerificationKeyStorage::<T>::get();
			ensure!(!vk.is_empty(), Error::<T>::VerificationKeyIsNotSet);
			let deserialized_vk = Verifier::from_json_u8_slice(vk.as_slice())
				.map_err(|_| Error::<T>::MalformedVerificationKey)?;
			Ok(deserialized_vk)
		}

		fn get_rotate_verifier() -> Result<Verifier, Error<T>> {
			let vk = RotateVerificationKeyStorage::<T>::get();
			ensure!(!vk.is_empty(), Error::<T>::VerificationKeyIsNotSet);
			let deserialized_vk = Verifier::from_json_u8_slice(vk.as_slice())
				.map_err(|_| Error::<T>::MalformedVerificationKey)?;
			Ok(deserialized_vk)
		}

		fn store_step_verification_key(vec_vk: Vec<u8>) -> Result<Verifier, Error<T>> {
			let vk: VerificationKeyDef<T> = vec_vk
				.try_into()
				.map_err(|_| Error::<T>::TooLongVerificationKey)?;
			let deserialized_vk = Verifier::from_json_u8_slice(vk.as_slice())
				.map_err(|_| Error::<T>::MalformedVerificationKey)?;
			ensure!(
				deserialized_vk.vk_json.curve == *"bn128",
				Error::<T>::NotSupportedCurve
			);
			ensure!(
				deserialized_vk.vk_json.protocol == *"groth16",
				Error::<T>::NotSupportedProtocol
			);

			StepVerificationKeyStorage::<T>::put(vk);
			Ok(deserialized_vk)
		}

		fn store_rotate_verification_key(vec_vk: Vec<u8>) -> Result<Verifier, Error<T>> {
			let vk: VerificationKeyDef<T> = vec_vk
				.try_into()
				.map_err(|_| Error::<T>::TooLongVerificationKey)?;
			let deserialized_vk = Verifier::from_json_u8_slice(vk.as_slice())
				.map_err(|_| Error::<T>::MalformedVerificationKey)?;
			ensure!(
				deserialized_vk.vk_json.curve == *"bn128",
				Error::<T>::NotSupportedCurve
			);
			ensure!(
				deserialized_vk.vk_json.protocol == *"groth16",
				Error::<T>::NotSupportedProtocol
			);

			RotateVerificationKeyStorage::<T>::put(vk);
			Ok(deserialized_vk)
		}

		fn verified_step_call(
			function_id: H256,
			input: ethabi::Bytes,
		) -> Result<VerifiedStepOutput, DispatchError> {
			let input_hash = sha2_256(input.as_slice());
			let verified_call = VerifiedStepCall::<T>::get();
			if verified_call.verified_function_id == function_id
				&& verified_call.verified_input_hash == H256(input_hash)
			{
				let trait_object: VerifiedStepOutput = verified_call.verified_output;
				Ok(trait_object)
			} else {
				Err(Error::<T>::StepVerificationError.into())
			}
		}

		fn verified_rotate_call(
			function_id: H256,
			input: ethabi::Bytes,
		) -> Result<U256, DispatchError> {
			let input_hash = sha2_256(input.as_slice());
			let verified_call = VerifiedRotateCall::<T>::get();

			if verified_call.verified_function_id == function_id
				&& verified_call.verified_input_hash == H256(input_hash)
			{
				Ok(verified_call.sync_committee_poseidon)
			} else {
				Err(Error::<T>::RotateVerificationError.into())
			}
		}
	}
}
