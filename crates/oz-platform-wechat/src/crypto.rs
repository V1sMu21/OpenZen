use aes::Aes128;
use aes::cipher::{BlockEncrypt, KeyInit};
use aes::cipher::generic_array::GenericArray;

pub fn aes_ecb_encrypt(data: &[u8], key: &[u8]) -> Vec<u8> {
    let key = GenericArray::from_slice(&key[..16]);
    let cipher = Aes128::new(key);
    let padded = pkcs7_pad(data, 16);
    let mut result = vec![0u8; padded.len()];
    for (i, chunk) in padded.chunks(16).enumerate() {
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        result[i * 16..(i + 1) * 16].copy_from_slice(&block);
    }
    result
}

pub fn random_key() -> [u8; 16] {
    let mut key = [0u8; 16];
    let mut state: u32 = 0;
    for b in key.iter_mut() {
        state = state.wrapping_mul(1103515245).wrapping_add(12345);
        *b = ((state >> 16) & 0xFF) as u8;
    }
    key
}

pub fn key_to_hex(key: &[u8; 16]) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

fn pkcs7_pad(data: &[u8], block_size: usize) -> Vec<u8> {
    let pad_len = block_size - (data.len() % block_size);
    let mut padded = data.to_vec();
    padded.extend(std::iter::repeat_n(pad_len as u8, pad_len));
    padded
}

