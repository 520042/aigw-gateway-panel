/**
 * POW Worker - Solves DeepSeek proof-of-work challenges using WASM.
 * 
 * Usage: echo '{"challenge":"...","salt":"...","difficulty":144000,"expire_at":...}' | node pow_worker.js
 * Output: {"answer": 12345} or {"error": "message"}
 */

const fs = require('fs');
const path = require('path');

const WASM_PATH = path.join(__dirname, 'wasm', 'sha3_wasm_bg.wasm');

async function loadWasm() {
    const wasmBuffer = fs.readFileSync(WASM_PATH);
    const wasmModule = await WebAssembly.compile(wasmBuffer);
    const instance = await WebAssembly.instantiate(wasmModule, {});
    return instance;
}

function writeToMemory(instance, text) {
    const encoded = new TextEncoder().encode(text);
    const length = encoded.length;
    const ptr = instance.exports.__wbindgen_export_0(length, 1);
    const memView = new Uint8Array(instance.exports.memory.buffer);
    memView.set(encoded, ptr);
    return [ptr, length];
}

async function solvePow(challengeConfig) {
    const instance = await loadWasm();
    
    const { challenge, salt, difficulty, expire_at } = challengeConfig;
    const prefix = `${salt}_${expire_at}_`;
    
    const retptr = instance.exports.__wbindgen_add_to_stack_pointer(-16);
    
    try {
        const [challengePtr, challengeLen] = writeToMemory(instance, challenge);
        const [prefixPtr, prefixLen] = writeToMemory(instance, prefix);
        
        instance.exports.wasm_solve(
            retptr,
            challengePtr,
            challengeLen,
            prefixPtr,
            prefixLen,
            difficulty
        );
        
        const memView = new DataView(instance.exports.memory.buffer);
        const status = memView.getInt32(retptr, true);
        
        if (status === 0) {
            return { error: "No solution found" };
        }
        
        const answer = memView.getFloat64(retptr + 8, true);
        return { answer: Math.floor(answer) };
        
    } finally {
        instance.exports.__wbindgen_add_to_stack_pointer(16);
    }
}

// Read input from stdin
let input = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => { input += chunk; });
process.stdin.on('end', async () => {
    try {
        const config = JSON.parse(input.trim());
        const result = await solvePow(config);
        process.stdout.write(JSON.stringify(result) + '\n');
    } catch (err) {
        process.stdout.write(JSON.stringify({ error: err.message }) + '\n');
        process.exit(1);
    }
});
