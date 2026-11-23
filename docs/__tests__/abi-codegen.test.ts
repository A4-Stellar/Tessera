import fs from 'fs';
import os from 'os';
import path from 'path';
import {
  extractModuleDoc,
  extractContractAbiFromSource,
  parseSorobanInspectOutput,
  extractContractAbi,
  extractWorkspaceAbi,
  generateRustBindings,
  generateTypeScriptTypes,
  generatePythonDataclasses,
  generateGoStructs,
  buildAllAbiArtifacts,
  syncGeneratedFiles,
} from '../scripts/lib/abi-codegen.mjs';

const REPO_ROOT = path.resolve(__dirname, '..', '..');

const SAMPLE_CONTRACT_RUST = `//! Sample Vault contract for tokenized RWA custody.
//! Supports deposits, withdrawals, and emergency pause.
#![no_std]

use soroban_sdk::{contract, contracterror, contractimpl, contracttype, Address, Env, String, Vec};

/// Current lifecycle status of a custody vault.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VaultStatus {
    /// Vault is accepting deposits.
    Active = 0,
    /// Vault is frozen by compliance.
    Frozen = 1,
}

/// Stored metadata for a vault position.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultPosition {
    /// Unique position identifier.
    pub id: u64,
    /// Holder wallet address.
    pub owner: Address,
    /// Deposited balance in stroops.
    pub balance: i128,
    /// Optional memo label.
    pub label: Option<String>,
}

/// Errors returned by the Vault contract.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum VaultError {
    /// Vault has already been initialized.
    AlreadyInitialized = 1,
    /// Caller is not authorized to perform this operation.
    Unauthorized = 2,
    /// Requested position does not exist.
    PositionNotFound = 3,
}

#[contract]
pub struct VaultContract;

#[contractimpl]
impl VaultContract {
    /// Initialize the vault with an administrator address.
    pub fn initialize(env: Env, admin: Address) -> Result<(), VaultError> {
        let _ = (env, admin);
        Ok(())
    }

    /// Deposit funds into a new or existing vault position.
    pub fn deposit(env: Env, owner: Address, amount: i128, tags: Vec<String>) -> Result<u64, VaultError> {
        let _ = (env, owner, amount, tags);
        Ok(1)
    }

    /// Fetch a vault position by its identifier.
    pub fn get_position(env: Env, position_id: u64) -> Option<VaultPosition> {
        let _ = (env, position_id);
        None
    }
}
`;

describe('Contract ABI & Multi-Language SDK Codegen Pipeline (#451)', () => {
  describe('extractModuleDoc & extractContractAbiFromSource', () => {
    it('extracts //! module doc comments and /// item doc comments into human-readable ABI descriptions', () => {
      const moduleDocInfo = extractModuleDoc(SAMPLE_CONTRACT_RUST);
      expect(moduleDocInfo.moduleDoc).toContain('Sample Vault contract for tokenized RWA custody.');
      expect(moduleDocInfo.moduleDoc).toContain('Supports deposits, withdrawals, and emergency pause.');

      const abi = extractContractAbiFromSource(SAMPLE_CONTRACT_RUST, {
        id: 'vault',
        contractName: 'VaultContract',
        pascalPrefix: 'Vault',
      });

      expect(abi.contractId).toBe('vault');
      expect(abi.contractName).toBe('VaultContract');
      expect(abi.doc).toContain('Sample Vault contract for tokenized RWA custody.');

      // Enums
      expect(abi.enums).toHaveLength(1);
      expect(abi.enums[0].name).toBe('VaultStatus');
      expect(abi.enums[0].doc).toBe('Current lifecycle status of a custody vault.');
      expect(abi.enums[0].variants).toEqual([
        { name: 'Active', tupleTypes: [], value: 0, doc: 'Vault is accepting deposits.' },
        { name: 'Frozen', tupleTypes: [], value: 1, doc: 'Vault is frozen by compliance.' },
      ]);

      // Structs
      expect(abi.structs).toHaveLength(1);
      expect(abi.structs[0].name).toBe('VaultPosition');
      expect(abi.structs[0].doc).toBe('Stored metadata for a vault position.');
      expect(abi.structs[0].fields).toEqual([
        { name: 'id', type: 'u64', doc: 'Unique position identifier.' },
        { name: 'owner', type: 'Address', doc: 'Holder wallet address.' },
        { name: 'balance', type: 'i128', doc: 'Deposited balance in stroops.' },
        { name: 'label', type: 'Option<String>', doc: 'Optional memo label.' },
      ]);

      // Errors
      expect(abi.errors).toEqual([
        {
          name: 'AlreadyInitialized',
          code: 1,
          doc: 'Vault has already been initialized.',
        },
        {
          name: 'Unauthorized',
          code: 2,
          doc: 'Caller is not authorized to perform this operation.',
        },
        {
          name: 'PositionNotFound',
          code: 3,
          doc: 'Requested position does not exist.',
        },
      ]);

      // Methods (Env parameter stripped, Result unwrapped)
      expect(abi.methods).toEqual([
        {
          name: 'initialize',
          doc: 'Initialize the vault with an administrator address.',
          inputs: [{ name: 'admin', type: 'Address' }],
          output: 'void',
        },
        {
          name: 'deposit',
          doc: 'Deposit funds into a new or existing vault position.',
          inputs: [
            { name: 'owner', type: 'Address' },
            { name: 'amount', type: 'i128' },
            { name: 'tags', type: 'Vec<String>' },
          ],
          output: 'u64',
        },
        {
          name: 'get_position',
          doc: 'Fetch a vault position by its identifier.',
          inputs: [{ name: 'position_id', type: 'u64' }],
          output: 'Option<VaultPosition>',
        },
      ]);
    });
  });

  describe('parseSorobanInspectOutput & soroban contract inspect --wasm integration', () => {
    it('parses soroban contract inspect --wasm JSON output and merges with source docs', () => {
      const inspectJson = JSON.stringify({
        entries: [
          {
            type: 'function',
            name: 'initialize',
            doc: 'Initialize from WASM spec entry.',
            inputs: [{ name: 'admin', type: 'Address' }],
            outputs: [],
          },
          {
            type: 'function',
            name: 'extra_wasm_fn',
            doc: 'Function discovered via soroban contract inspect --wasm.',
            inputs: [{ name: 'flag', type: 'bool' }],
            outputs: [{ type: 'u32' }],
          },
        ],
      });

      const parsed = parseSorobanInspectOutput(inspectJson);
      expect(parsed).not.toBeNull();
      expect(parsed?.methods).toHaveLength(2);
      expect(parsed?.methods[1]).toEqual({
        name: 'extra_wasm_fn',
        doc: 'Function discovered via soroban contract inspect --wasm.',
        inputs: [{ name: 'flag', type: 'bool' }],
        output: 'u32',
      });

      const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'tessera-abi-test-'));
      const fakeWasmPath = path.join(tmpDir, 'registry.wasm');
      fs.writeFileSync(fakeWasmPath, Buffer.from([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]));

      try {
        const mockExecSync = jest.fn().mockReturnValue(inspectJson);
        const abi = extractContractAbi({
          repoRoot: REPO_ROOT,
          contractSpec: {
            id: 'registry',
            contractName: 'RegistryContract',
            pascalPrefix: 'Registry',
            relativePath: 'contracts/registry/src/lib.rs',
          },
          wasmPath: fakeWasmPath,
          execSyncImpl: mockExecSync,
        });

        expect(mockExecSync).toHaveBeenCalledWith(
          expect.stringContaining('soroban contract inspect --wasm'),
          expect.any(Object)
        );
        expect(abi.methods.some((m: { name: string }) => m.name === 'extra_wasm_fn')).toBe(true);
        expect(abi.methods.some((m: { name: string }) => m.name === 'register_asset')).toBe(true);
      } finally {
        fs.rmSync(tmpDir, { recursive: true, force: true });
      }
    });
  });

  describe('extractWorkspaceAbi & multi-language code generators', () => {
    it('extracts all 4 workspace contracts and generates Rust, TypeScript, Python, and Go types', () => {
      const workspaceAbi = extractWorkspaceAbi(REPO_ROOT);
      expect(workspaceAbi.contracts).toHaveLength(4);

      const contractIds = workspaceAbi.contracts.map((c: { contractId: string }) => c.contractId);
      expect(contractIds).toEqual(['registry', 'compliance', 'dividend', 'asset-token']);

      for (const contract of workspaceAbi.contracts) {
        expect(contract.doc.length).toBeGreaterThan(20);
        expect(contract.methods.length).toBeGreaterThan(5);
        expect(contract.errors.length).toBeGreaterThan(2);
      }

      const rustCode = generateRustBindings(workspaceAbi);
      expect(rustCode).toContain('pub mod registry');
      expect(rustCode).toContain('pub mod compliance');
      expect(rustCode).toContain('pub mod dividend');
      expect(rustCode).toContain('pub mod asset_token');
      expect(rustCode).toContain('pub const ALL_CONTRACTS: &[ContractAbiSpec]');

      const tsCode = generateTypeScriptTypes(workspaceAbi);
      expect(tsCode).toContain('export interface RegistryAssetEntry');
      expect(tsCode).toContain('export interface ComplianceKycRecord');
      expect(tsCode).toContain('export interface DividendDistribution');
      expect(tsCode).toContain('export interface AssetTokenAssetMetadata');
      expect(tsCode).toContain('export interface AssetTokenContractAbi');

      const pyCode = generatePythonDataclasses(workspaceAbi);
      expect(pyCode).toContain('class RegistryAssetEntry:');
      expect(pyCode).toContain('class ComplianceKycRecord:');
      expect(pyCode).toContain('class DividendDistribution:');
      expect(pyCode).toContain('class AssetTokenError(IntEnum):');

      const goCode = generateGoStructs(workspaceAbi);
      expect(goCode).toContain('package generated_test');
      expect(goCode).toContain('type RegistryAssetEntry struct');
      expect(goCode).toContain('type ComplianceKycRecord struct');
      expect(goCode).toContain('type DividendDistribution struct');
      expect(goCode).toContain('type AssetTokenAssetMetadata struct');
    });
  });

  describe('syncGeneratedFiles drift verification (--check)', () => {
    it('verifies generated repository files are in sync and detects drift when a file is modified', () => {
      const { files } = buildAllAbiArtifacts(REPO_ROOT);
      const checkResult = syncGeneratedFiles(files, { checkOnly: true, repoRoot: REPO_ROOT });
      expect(checkResult.inSync).toBe(true);
      expect(checkResult.driftedFiles).toEqual([]);

      // Simulate drift in a temporary directory
      const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'tessera-drift-test-'));
      const sampleFile = path.join(tmpDir, 'contract_types.ts');
      try {
        const testMap = new Map([[sampleFile, '// Expected content\nexport const OK = true;\n']]);

        // Missing file -> drift detected
        const missingCheck = syncGeneratedFiles(testMap, { checkOnly: true, repoRoot: tmpDir });
        expect(missingCheck.inSync).toBe(false);
        expect(missingCheck.driftedFiles).toContain('contract_types.ts');

        // Write files via syncGeneratedFiles
        const writeResult = syncGeneratedFiles(testMap, { checkOnly: false, repoRoot: tmpDir });
        expect(writeResult.inSync).toBe(true);
        expect(writeResult.updatedFiles).toContain('contract_types.ts');

        // Check again -> in sync
        const syncedCheck = syncGeneratedFiles(testMap, { checkOnly: true, repoRoot: tmpDir });
        expect(syncedCheck.inSync).toBe(true);

        // Tamper with file -> drift detected
        fs.writeFileSync(sampleFile, '// Tampered content\n', 'utf8');
        const tamperedCheck = syncGeneratedFiles(testMap, { checkOnly: true, repoRoot: tmpDir });
        expect(tamperedCheck.inSync).toBe(false);
        expect(tamperedCheck.driftedFiles).toContain('contract_types.ts');
      } finally {
        fs.rmSync(tmpDir, { recursive: true, force: true });
      }
    });
  });
});
