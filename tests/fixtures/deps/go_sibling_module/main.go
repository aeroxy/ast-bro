package main

import (
	"fmt"

	"example.com/sibling/sub/inner"
)

func main() {
	fmt.Println(inner.Greet("world"))
}
