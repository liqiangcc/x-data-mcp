// GBBQ record decrypt + event decode — Node port of src/gbbq.rs
// (decrypt_record, event_from_clear, decode_events).
//
// Semantics mirrored verbatim:
//   - decrypt_record: pinned 4176-byte key table; per 8-byte block a
//     Feistel loop j = 0x40..4 step -4 over u32 LE words, u32 wrapping
//     arithmetic; final 5 bytes copied raw
//   - event_from_clear: market=clear[0], code=UTF-8(clear[1..8]) trimmed
//     of trailing NUL (strict UTF-8, fail-closed on invalid bytes with
//     Rust verbatim error text), date_raw=u32le(clear[8..12]),
//     category=clear[12], c_raw=f32le(clear[13..29])
//   - lossless: c_raw_bits exposed as u32 alongside widened f64 values
//   - typed boundary: records must be Uint8Array of exactly 29 bytes

const KEY_TABLE_HEX =
  `38a7c21de06a17e2d139a2409cba46af42c6ff0574eadabb89b4f844ac89d7f2987fb6bce4f76b750504586779c86dc6
2b06968cfb86068bbfd6e8e187496b36c71802795325727213cc040b90240cdcdb031ad52e04855c7e8ebd02262dbd06
1b5034991ba22404f28835c889ead5fb1224bbb53b29ca14a604cea9a85802b9aae397a3a62257bbada0225feb058611
c3edb13f39c236d14a43c8644db06e3a7c516df78ec6dff38ea41e749db222054d073f967f97f963b9c42b9875f6d684
56dc15d3528b60f3d60ea9ad0707e9028658c2329c90bcc919bfb0547af8cca827638229eefb9811bf352962919395fc
f4f008e4b23ab45eb3b02e3e20c1d743597dc6295f69747fb277e10efa85a1c9777383b3cb1c60dbe95369fcb3185915
0f978a7ac883f549dc1b3e86c1954546e216677f1235a0bb27fbccf8307e4fc86dab18b20d01cc7920807bfa37aa149e
85e825e9d42d354e8fd3deb0068d15155265e8390328090267993d13baf3685c4c89b0e36bae165c8825f8330319025b
297b2a412d7549489bb3b6b3bfaadf8c95fe0f13b87b02bb52e11c34c39b8759e246cc22774bd7c42c31aa847c445188
151accae409d1f4497299845607447a10da573f053ff01f9f49af13607d02da0792d812325ad4b9cc8bc12554dd4bb95
b1b9be7da6e6a053ba838cdd7ee94bedba2842d8ff986935ca4e9c9d57d6cfa0895ca2e754d2af4cfb54c4b44fc3baf8
a2586919790ea80e3dc804fd2632c8e1028ba71cc39125e5d849dbdf195f16f5a78b182304d4bffb44c4617c796ec890
15b5eb5087ca7a69472fafa8b5a28a84c44179e8de0cacd0d56f34c6cba776f900244205267e7b1486597bdb1c62d5b7
3ef71744274bd2c66fffc84955ad65522d43c2339b63ab3d545428e20265039a034b8f641a9252de32d62bf0bebe1d54
b17c70419b9055da715521b9b66890195fbcaab4550ee6814ca3bebc64d7590059bd0f6a571aa6a0d51a0a80d3090673
5a51e2dd2966aca08629212b7a6d9e3a68d0a3dca72b85a04cd4f0c5c443e4cf0c198130b6f6be71f5ac25aacf429006
641b4529fd3aa3b60b9d299ffa31b86dd8ec43f5927e3522e0c3d309066171dae8360a19f62381cb89e0676efeb1e647
72635c2518e0b46585efb51b26239089cceee301779563dfc4acbfe637149915498a960291aa1d9821575e8796c7b587
083f58065258178faba84ea17a60b1695e9cbee2d0c51259df31ebd2195496e210118e68b41a2dd32fab12f7fef3a7f7
61fcf77ccbfc878c6a1040297b30d60d134c71cd5eab36a2f14c05ed5388e5ff8e71795db5afd3676dc4446babc1a7aa
38d8701e08e6d2367b881196dbd268d9ffd8502b3aa9cc451acacdd205c6fca0350cee982b5cb2396a27128f97eccb7b
b6c027f6a74875098298ca3a5de3960ca5d2b36ca4d11fae9967b03dd69a7a3e008bfd4532f79f287c9403db64aa4480
d227afb373875731eb08d9ba734d2c7703bff50f473c22da3fb9f19a1b228316eef418fc08e83b301c0450aa4ce32853
abdef85f32d9e1787bf1c5a8ca85b69f891f40b82c88d7c1663445d646fd7bf372a3325523cfb5b079aba0f1005cdbee
3f51aaaec0898e47a5304e4bddd6aed86d401c4e8efb0c608d541e2f17b73aeddedc81f57285b7a639316f47508443c5
11f36a268eba7f819831fd136b83c911614864fae3f5392c1211c16d4d0313a6c2e0dff5328e5b35a77f08f785270d71
9db8ce9c1eba773af6a1a7269429c0201065756eefaa320c66913a4e0e74e28afeb6f817c7a7e4d835672ef083a89fa6
281340a396dc498355e185abbd4ded88fa3669a977595a9cd0a0b13deb3116dc3e297b39015bd4ff5ce59edaf755d53f
e33b5176838e40aee12ee83ef808b7b0242691ad824c2e2f377a34a105bd8c9a75525ccd5980cb92f8b1f8a5f22c9f4a
59bfef76a3744fe1c97c7f91d90d1205b28ed0e0bb46d45c442f656d7a1c0286fb7e7db62a57b9db80cd02bfe79e3521
fbbe2813829ff074f79255def27bf2f27df5a0140f994d25f4dc11177a776577ccbeef9088e8fdb24e8ef526fe535d65
a974470bcbe9e8719595876cfd8694a7e5fc20001e0a0ae3851724d4d0738a111e1eef83e3d7e1bfcc98076d70373a8f
3117554e60a8c8ab4f082d3776e62b58dd810fd16e9aa6553d8082999e2d169adf4ecb3b5ddaa85308c7ff54ddc61131
1ab6eba303084afbb445ecc07c0dc6cfcb1b7846888ff46a15622f1712e6416476589678db29b56aaede63416fbe9b37
6cc9d0ec1bf679179efe790eb18228f20615c2be969ce08180d700db95874bc00d91555b1f86226474ea1b8985d2ddf7
9ff1d9090664fa6d5972efce66a703d199e8dfaed7635f605fab6ec522c83a946a3b0072f8db90e705dca2890f83aa03
fe42141c8ae61c9edbd8d0ca97216caded0ae0a29eecc1ffd1b48a9aadab340b133fb5188d859e0df9fbac212edd7ade
bf9f7ebdbf84dff5fd1ebee11f0ff8189d73090229b75b267e4475044db1aa2f3adb463812d14135912906dfc9986992
02f24812a971d2ae3b236d1ce26b8b75874a13a71f814d2965530a3a34ce6de6318d7e4edd256e7644823c47364cb9c4
9bf44f84431156c294537eb02e36daeb775fc164e2ca9fbe29d8063653d06f8219dabc8c5f4d45e721379e90a6d433a8
644decbc905efe8e8bca177cffac96bb21cf3d24713bc2a1746885cf328e7f6339c5e78ea5e0cd3af59ab8fd43d44339
088e45765fdfe917545912edd0e93d6f3f02148a0a479ad1e7fa4ea1410050ef609d4dc1ca879840e7b20f76c09d71ef
d74693c12b9f11b8f905aceda7726bf5119b3e0a04217d06d746767badae9d95a6476805adf5387cc7a55acab2cb4818
c1f262559836390880c528b106e4fb46113c38a14f1cfea181b7fcdb94b07afeb574f1bb92aaffb0fe1e318bc6bcf04f
1afe91c57a9c73094a329051018b12c020ca3ccb1483d3c77c5a1279ee561a36c409e23edce8cef1c1a19e99da644fcf
1ed62b7027863ecfbe751c399bf95363c16b58cc71d2074188bb147096f168ce1375fef4a0c885a2671849560d07941d
7461890c32499d0d94734aab1ae90fe0bab64a34f9331db371c2b864d70bcb19f7bde0693e2496b1c428095f58ae8ac0
839919644d443755a69ba1425084b81829b52191582388eb8f134a2409ec0f6d7daf3efcf7f39f343915c48403bb7e67
395f2a2c6794f4a6b5023f4556790c2a9b257767c23bccf2713b4f832a8d8c530d184954ca580ebe8b3a5374fc6f4728
078ec1f553d3344b0805ffe91429401b57ad77ece8dada3555a77803564c7cb2ed3bb5616591df41b45dc9b79b138241
15d7b36e1cc815b4f0f33f914ba1c8907891395a2155da6ae12cbac93869f6aea82b8cb714c1358235a0784756c09aa7
7f74146485f1b748bc558c6aa4951ccbf352f9546115275643d02795e335aa39dc2338daef1f27653aabf7ccbb25db00
363496d1f7c4ec4437427e171867c89c9a5b39085c3cf492f1163188fa12449e79271cc20b46accd1f39b89f9a56340a
8586c2b1b19b31ce4757053ea7ae3f3e012dc5b9c1cbbaab0a2ad271e4ecf80a7185cca1ca6eef9d8722385d8081f71a
6c317b8286bd7f109d89b6f7afe4410d4f97288034063e193a2160ed5418020f2fd5d53ba5870121381ba6993228e98d
6f02356085bd64c4b0267e68d1e697b5326eb24feb064c4dc2978e6b3022c0b43d47937867ac2742dd5c3c27ed0a6ce4
4a0d0fdf5263a6707609f02e58f605b2dfeec91fcb1d110ca18b1926b8102c8148ff98ef30360c01c54ad9ac057289c7
3fd64de017babab3d3e81b0c8cc8df6bfe7eba91fdf6a0cb5919b0012fd70ba0620f5fce74b8eb4289b5becac9efda9a
bbc6661be065eed43aced9cc0ebb8550414501ba1b29116f34115503dd0cb599563a934d4d956dcec351e015543eff2f
a3da59ec3d592d62fc6439d67bc880781dd7fde80b5d8aed1a9d98cbc2ee784730ad8f64a5821223dab33eca4c857a80
d59f4620d6eed1f933fa1fc59c8ef91e6651a54668dcb77fa85adee618d78c2b5deaa8ec6b8b48c1925ac1b16a5e3782
224b6ab6f040168916a581f8d41b20268635e5adc1016ec9b5d069c50b3108515d35fc74f513047af45710535ba4cc8b
218282154b8c3d6bda9185cbd6cf0580d0f0cf0ddf7ab499c7f8d54c765630e965b65860c1c0398a4254bc4a488ba1d9
5c32057a1cbb50515b7fc7752d6855e6837bc398fde6d5b8daa8310178f5608b1ad2fd513447faaf23aee2de15a70766
69359a406155259823542a50c97da6ce74f8190c8e63e5492ff91705fd391555f4b091bf60b7b2402e7ad36886c0fc38
88abb9038a04051a9f61aef2d3b8a429f85143cf84264a906e1327af7b52dbf900e8aec0b56f64035720597cf5e165a8
47c3bdee722a85e2708dea9d98d42ad570a2e976a2dae67cb0f714d923b688c0b36f4212f4690c1581d6f70bb71bdf15
e675631353b32043799034e3344880d686bb45a285ddf823643bd568ab995334c6250a877317375639ba8c0e39244bcc
aa98840c2f27e6e2ac86345d1e25aefd1eff3c27ad26184a1ae509615d835f2cdc41a7c607555bb50b71fe86e730a1bc
27af5f24511add20f6329e3d646fdc43652a80cb95c4b6f0e1f3cf6cf2c29cea81880c2dd2da7482c6a51e98d3bc71ed
e20b05dabb0efa350a2cd5c862e7b1af95146c837df1ce9f136bd868c9a5f5872ea58fd75cb2c69937315aa4d0e243df
c8bebd10c0d8226395461ee78ca861e474026cb430f3061511e62a3a0d3b2fb93bb383401879fb3938b7ce4dbaf69eaa
e18f321cb168dd5c2c376561733dc63456cdeabc776aa17d6af1f978af0fd9c2aad3d7a82da86ebc198396b5a33eb3b2
5c54ad77ce1de5d5aab30d367a327d5ca360668d84a0bd4f0fa90989b8ec148a2b2b748e75775a8eb251d026d6068c9a
ca31d69417f014d7431c820c0083e675055c52ab0c388fa3357752e83e3bcb4881e325b1a94012764f16f1ce3dd72389
44d73f247eb74666c1167a17b22a99f1ac3cc99dc5fe89bebf2c68bc2ca7f1c52f261eccd1af7daa7dc5944a4dc48797
2d2b6a5e5ebf398218ab8cb9dc8083a1d180d265fe2ecc6af10284b2366037244e5e57ada5c5501a5ea45c31b6936057
acebed653fbfeac708ca130093e5e679f63720cab46e399e834f158b15cde78c9093b085919bae21ef03d0a4b62ab4c6
d307049254728eec2eb3476cce42067fe05b96f2488bfa8f83e24710a5b730f868b0fd02746f4871d7f12edfa1526176
9947be0a2ff8f2699dad03fae684a7cf357d8f5fc5a69b216635bc58d589b5e09f11f0a88a1fc83c24b2b7f16c8adb3b
397acad0ef15612272fdfc023dbd76359ee1c6d72cb259e103e0ff7a8703799f61abcc4998c241cf6e9baa529bd008b5
9e23f6c1398277165dd4e1b3ada00c58f8e267006a0b4bd26ce1c56b9dba3f4082c528b8c1607585eec4fa04ed6264b6
2910674b9bd66c0e06626483caf02f2db8f60ad7d76a1c5814be1860802902cdf6b195a56d2e279c08e31fc5c2077f63
7fdb82c6c685aca6d24cf17fdb1dcf86205660c024e0c0420b4e005f8b7860feeaec6d31934970eb2a454f929b6c1728
bb89fcc00784ccad1b85f285185c3d5a6054af039d9ee426d386aa0b7ca3329cc20f3ad43e1f5243a831e970fc0cb47c
f5e3c76f11ed224c0c1b82cb72a495281ad41be5c46ed7f1ecbf252cb89287a8d215793439c0be0dc8682df2d38e0109
3c4894326989d5c05de82ce6a697594b9ac661b09edb81dcd3f947348400ca87be5d6d56f301023bffffffff00000000`.replace(/\s/g, "");

const keys = (() => {
  const k = new Uint8Array(KEY_TABLE_HEX.length / 2);
  for (let i = 0; i < k.length; i++)
    k[i] = parseInt(KEY_TABLE_HEX.slice(i * 2, i * 2 + 2), 16);
  return k;
})();
const kv = new DataView(keys.buffer);
const keyU32 = (off) => kv.getUint32(off, true);

export class GbbqDecodeError extends Error {
  constructor(message, kind, extra = {}) {
    super(message);
    this.name = "GbbqDecodeError";
    this.kind = kind;
    Object.assign(this, extra);
  }
}

const checkRecord = (record) => {
  if (!(record instanceof Uint8Array) || record.length !== 29) {
    throw new GbbqDecodeError(
      `GBBQ record must be a 29-byte Uint8Array, got ${record?.length ?? typeof record}`,
      "invalid_input");
  }
};

// decrypt_block: 8-byte block -> 8-byte clear block (u32 wrapping via >>>0).
function decryptBlock(record, start, out) {
  const bv = new DataView(record.buffer, record.byteOffset, record.byteLength);
  let num = (keyU32(0x44) ^ bv.getUint32(start, true)) >>> 0;
  let numold = bv.getUint32(start + 4, true);
  for (let j = 0x40; j >= 4; j -= 4) {
    let eax = keyU32(((num & 0xff0000) >>> 16) * 4 + 0x448);
    eax = (eax + keyU32((num >>> 24) * 4 + 0x48)) >>> 0;
    eax ^= keyU32(((num & 0xff00) >>> 8) * 4 + 0x848);
    eax = (eax + keyU32((num & 0xff) * 4 + 0xc48)) >>> 0;
    eax ^= keyU32(j);
    const old = num;
    num = (numold ^ eax) >>> 0;
    numold = old;
  }
  numold = (numold ^ keyU32(0)) >>> 0;
  const ov = new DataView(out.buffer);
  ov.setUint32(0, numold, true);
  ov.setUint32(4, num, true);
}

/// Decrypt one 29-byte record to its clear bytes (fresh Uint8Array).
export function decryptRecord(record) {
  checkRecord(record);
  const clear = new Uint8Array(29);
  for (let block = 0; block < 3; block++) {
    const tmp = new Uint8Array(8);
    decryptBlock(record, block * 8, tmp);
    clear.set(tmp, block * 8);
  }
  clear.set(record.subarray(24, 29), 24);
  return clear;
}

const utf8 = new TextDecoder("utf-8", { fatal: true });

/// Decode one 29-byte clear record into a typed event.
/// Mirrors event_from_clear; error text byte-identical for invalid code.
export function eventFromClear(index, clear) {
  checkRecord(clear);
  const codeBytes = Array.from(clear.subarray(1, 8));
  let code;
  try {
    code = utf8.decode(clear.subarray(1, 8)).replace(/ +$/, "");
  } catch {
    throw new GbbqDecodeError(
      `GBBQ record ${index} code bytes are not UTF-8: [${codeBytes.join(", ")}]`,
      "invalid_code", { index, bytes: codeBytes });
  }
  const cv = new DataView(clear.buffer, clear.byteOffset, clear.byteLength);
  const cRaw = [], cRawBits = [];
  for (let i = 0; i < 4; i++) {
    const off = 13 + i * 4;
    cRawBits.push(cv.getUint32(off, true));
    cRaw.push(cv.getFloat32(off, true));
  }
  return {
    market: clear[0],
    code,
    date_raw: cv.getUint32(8, true),
    category: clear[12],
    c_raw: cRaw,
    c_raw_bits: cRawBits,
  };
}

/// Decrypt + decode one encrypted record.
export function decodeRecord(index, record) {
  checkRecord(record);
  return eventFromClear(index, decryptRecord(record));
}

/// Decode every record of a parsed container, preserving order.
/// Accepts {records: Uint8Array[]} from gbbq_file.parse.
export function decodeEvents(file) {
  if (!file || !Array.isArray(file.records)) {
    throw new GbbqDecodeError("expected a parsed RawFile {records}", "invalid_input");
  }
  return file.records.map((record, index) => decodeRecord(index, record));
}
