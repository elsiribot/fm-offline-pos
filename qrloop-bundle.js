// Minimal self-contained qrloop bundle for browser use.
// Provides window.qrloop = { dataToFrames, parseFramesReducer, areFramesComplete, framesToData, progressOfFrames }

(function() {
  // MD5 via SparkMD5 (loaded from CDN)
  function md5(bytes) {
    const arr = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
    return SparkMD5.ArrayBuffer.hash(arr.buffer.slice(arr.byteOffset, arr.byteOffset + arr.byteLength));
  }

  function md5Bytes(bytes) {
    const hex = md5(bytes);
    const result = new Uint8Array(16);
    for (let i = 0; i < 16; i++) result[i] = parseInt(hex.substr(i * 2, 2), 16);
    return result;
  }

  const MAX_NONCE = 10;
  const FOUNTAIN_V1 = 100;

  function cutAndPad(data, size) {
    const numChunks = Math.ceil(data.length / size);
    const chunks = [];
    for (let i = 0, o = 0; i < numChunks; i++, o += size) {
      chunks.push(data.slice(o, o + size));
    }
    const last = numChunks - 1;
    const pad = size - chunks[last].length;
    if (pad > 0) {
      const padded = new Uint8Array(size);
      padded.set(chunks[last]);
      chunks[last] = padded;
    }
    return chunks;
  }

  function xor(buffers) {
    const result = new Uint8Array(buffers[0]);
    for (let i = 1; i < buffers.length; i++) {
      for (let j = 0; j < buffers[i].length; j++) result[j] ^= buffers[i][j];
    }
    return result;
  }

  function wrapData(data) {
    const wrapped = new Uint8Array(20 + data.length);
    new DataView(wrapped.buffer).setUint32(0, data.length, false);
    wrapped.set(md5Bytes(data), 4);
    wrapped.set(data, 20);
    return wrapped;
  }

  function makeDataFrame(data, nonce, totalFrames, frameIndex) {
    const frame = new Uint8Array(5 + data.length);
    frame[0] = nonce;
    new DataView(frame.buffer).setUint16(1, totalFrames, false);
    new DataView(frame.buffer).setUint16(3, frameIndex, false);
    frame.set(data, 5);
    return btoa(String.fromCharCode(...frame));
  }

  function makeFountainFrame(dataChunks, selectedFrameIndexes) {
    const k = selectedFrameIndexes.length;
    const head = new Uint8Array(3 + 2 * k);
    head[0] = FOUNTAIN_V1;
    new DataView(head.buffer).setUint16(1, k, false);
    const selectedData = [];
    for (let j = 0; j < k; j++) {
      selectedData.push(dataChunks[selectedFrameIndexes[j]]);
      new DataView(head.buffer).setUint16(3 + 2 * j, selectedFrameIndexes[j], false);
    }
    const data = xor(selectedData);
    const frame = new Uint8Array(head.length + data.length);
    frame.set(head); frame.set(data, head.length);
    return btoa(String.fromCharCode(...frame));
  }

  function makeLoop(wrappedData, dataSize, index, random) {
    const nonce = index % MAX_NONCE;
    const dataChunks = cutAndPad(wrappedData, dataSize);
    const fountains = [];
    if (dataChunks.length > 2) {
      const fcount = Math.floor(dataChunks.length / 6);
      const k = Math.ceil(dataChunks.length / 2);
      for (let i = 0; i < fcount; i++) {
        const distribution = Array(dataChunks.length).fill(null)
          .map((_, i) => ({ i, n: random() }))
          .sort((a, b) => a.n - b.n)
          .slice(0, k)
          .map(o => o.i);
        fountains.push(makeFountainFrame(dataChunks, distribution));
      }
    }
    const result = [];
    let j = 0;
    const fountainEach = fountains.length > 0 ? Math.floor(dataChunks.length / fountains.length) : Infinity;
    for (let i = 0; i < dataChunks.length; i++) {
      result.push(makeDataFrame(dataChunks[i], nonce, dataChunks.length, i));
      if (i % fountainEach === 0 && fountains[j]) result.push(fountains[j++]);
    }
    return result;
  }

  function dataToFrames(data, dataSize, loops) {
    dataSize = dataSize || 120;
    loops = loops || 1;
    let seed = 1;
    function random() { let x = Math.sin(seed++) * 10000; return x - Math.floor(x); }
    const input = data instanceof Uint8Array ? data : new TextEncoder().encode(data);
    const wrappedData = wrapData(input);
    let r = [];
    for (let i = 0; i < loops; i++) r = r.concat(makeLoop(wrappedData, dataSize, i, random));
    return r;
  }

  // Importer
  const initialState = { frames: [], fountainsQueue: [], exploredFountains: [] };

  function resolveFountains(state) {
    if (!state) return state;
    const fountainsQueue = state.fountainsQueue.slice(0);
    const frames = state.frames.slice(0);
    if (fountainsQueue.length === 0 || frames.length === 0) return state;
    const framesCount = frames[0].framesCount;
    const framesByIndex = {};
    for (const frame of frames) framesByIndex[frame.index] = frame;
    let i = 0;
    while (i < fountainsQueue.length) {
      const fountain = fountainsQueue[i];
      const existingData = [], missing = [];
      for (const idx of fountain.frameIndexes) {
        if (framesByIndex[idx]) existingData.push(framesByIndex[idx].data);
        else missing.push(idx);
      }
      if (existingData.length > 0 && fountain.data.length !== Math.min(...existingData.map(f => f.length))) {
        fountainsQueue.splice(i, 1);
      } else if (missing.length === 0) {
        fountainsQueue.splice(i, 1);
      } else if (missing.length === 1) {
        const index = missing[0];
        const recovered = xor(existingData.concat([fountain.data]));
        const frame = { index, framesCount, data: recovered };
        frames.push(frame); framesByIndex[index] = frame;
        fountainsQueue.splice(i, 1);
        i = 0;
      } else { i++; }
    }
    return { ...state, frames, fountainsQueue };
  }

  function parseFramesReducer(_state, chunkStr) {
    const state = _state || initialState;
    let raw;
    try { raw = atob(chunkStr); } catch(e) { return state; }
    if (raw.length < 5) return state;
    const chunk = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i++) chunk[i] = raw.charCodeAt(i);
    const version = chunk[0];
    if (version === FOUNTAIN_V1) {
      if (state.exploredFountains.includes(chunkStr)) return state;
      if (chunk.length < 3) return state;
      const k = new DataView(chunk.buffer).getUint16(1, false);
      if (chunk.length < 3 + 2 * k) return state;
      const frameIndexes = [];
      for (let i = 0; i < k; i++) frameIndexes.push(new DataView(chunk.buffer).getUint16(3 + 2 * i, false));
      const data = chunk.slice(3 + 2 * k);
      return resolveFountains({
        frames: state.frames,
        fountainsQueue: state.fountainsQueue.concat({ frameIndexes, data }),
        exploredFountains: state.exploredFountains.concat(chunkStr)
      });
    }
    if (version >= MAX_NONCE) throw new Error("version " + version + " not supported");
    const framesCount = new DataView(chunk.buffer).getUint16(1, false);
    const index = new DataView(chunk.buffer).getUint16(3, false);
    const data = chunk.slice(5);
    return resolveFountains({
      ...state,
      frames: state.frames.filter(c => c.index !== index && c.framesCount === framesCount).concat({ framesCount, index, data })
    });
  }

  function areFramesComplete(s) {
    if (!s || s.frames.length === 0) return false;
    return s.frames[0].framesCount === s.frames.length;
  }

  function progressOfFrames(s) {
    if (!s || s.frames.length === 0) return 0;
    return s.frames.length / s.frames[0].framesCount;
  }

  function framesToData(s) {
    if (!s) throw new Error("frames is undefined");
    const sorted = s.frames.slice(0).sort((a, b) => a.index - b.index);
    let totalLen = 0;
    for (const f of sorted) totalLen += f.data.length;
    const all = new Uint8Array(totalLen);
    let offset = 0;
    for (const f of sorted) { all.set(f.data, offset); offset += f.data.length; }
    const length = new DataView(all.buffer).getUint32(0, false);
    const expectedMD5 = Array.from(all.slice(4, 20)).map(b => b.toString(16).padStart(2, '0')).join('');
    const data = all.slice(20, 20 + length);
    if (md5(data) !== expectedMD5) throw new Error("md5 doesn't match");
    return data;
  }

  window.qrloop = { dataToFrames, parseFramesReducer, areFramesComplete, framesToData, progressOfFrames };
})();
