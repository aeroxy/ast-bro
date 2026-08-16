package p

import "fmt"

type Root interface{ M() }

type Other interface{ N() }

type Leaf struct{ Root }

// Several embedded types, one of them qualified.
type Hard struct {
	Root
	Other
	fmt.Stringer
}
