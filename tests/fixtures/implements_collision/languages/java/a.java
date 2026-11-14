package p;

import java.util.List;

public interface Root {}

public interface Other {}

public class Leaf implements Root {}

/** Several bases, one generic, one qualified. */
public class Hard extends java.util.AbstractList<String> implements Root, Other {}
