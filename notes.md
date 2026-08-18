First 4 bytes are `ChunkID`, just ASCII `RIFF`
Next 4 bytes are `ChunkSize`, the size of the entire file, not including these 8 bytes already read
These two make up the "master RIFF chunk" which is said to contain the rest of the chunks

Next up we have a wave chunk:
4 bytes of `WAVE ID`, just ASCII `WAVE`
Which then "contains" a bunch of other chunks, the only one we care about is the format chunk:
4 bytes of `ChunkID`, just ASCII `fmt ` (note the space)