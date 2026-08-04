package main

import (
	"fmt"

	"example.com/multi/tools/helper"
	"example.com/multi/util"
)

func main() {
	fmt.Println(helper.Shout(util.Greet("world")))
}
