// Package docs exercises every place a Go doc comment can sit.
package docs

// Colors are the palette the renderer accepts.
// Adding one here does not register it with the theme loader.
const (
	// Red is the default.
	Red  = "red"
	Blue = "blue"
)

// Limits bound every request this package issues.
var (
	MaxBytes = 1 << 20
	MaxHops  = 3
)

// Standalone is not in a group.
const Standalone = 1

// Deprecated: use Standalone instead.
const (
	Ancient = 0
)

// Deprecated: use Standalone instead.
var (
	Obsolete = 0
)

/* Codes are the wire codes.

Adding one is a protocol change.
*/
const (
	CodeOK = 200
)

// Shapes are the shapes the renderer can draw.
type (
	// Circle is round.
	Circle struct {
		// Radius carries the unit in metres.
		Radius int
	}
	Square struct{}
)

// Request is what the client sends.
type Request struct {
	// Path is the request target.
	Path string
	Size int // Size is measured in bytes.
	Hops int
}

// Client talks to the server.
type Client interface {
	// Do sends one request.
	Do(r Request) error
	Close() error
}

// Send delivers a request.
func Send(r Request) error { return nil } // trailing comment on Send

func Receive() error { return nil }
