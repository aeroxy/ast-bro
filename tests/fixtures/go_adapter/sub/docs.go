// Package sub declares Red on the line where the parent fixture declares
// it, outside any block. `surface` walks into this directory and `map` of
// the parent file does not, so a test joining the two outputs has to
// compare whole paths.
package sub

// Red is not in a group.
const Red = "red"
