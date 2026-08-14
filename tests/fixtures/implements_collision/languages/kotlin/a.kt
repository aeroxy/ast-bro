package p

interface Root

interface Other

class Leaf : Root

/** A delegated base, a generic one, and a constructor call. */
class Hard(d: Other) : ArrayList<String>(), Root, Other by d
