package p

trait Root

trait Other

class Leaf extends Root

/** `extends` plus `with`, and a generic base. */
class Hard extends scala.collection.mutable.ListBuffer[String] with Root with Other
