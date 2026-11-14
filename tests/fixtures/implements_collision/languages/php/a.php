<?php

namespace P;

interface Root {}

interface Other {}

class Leaf implements Root {}

/** A leading-backslash base and a namespace-relative one. */
class Hard extends \P\Leaf implements namespace\Other {}
