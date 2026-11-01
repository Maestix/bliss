# The engine's entry points are found by name from libbliss.so, and the
# engine calls back into Native.ownerOf by name.
-keep class app.bliss.Native {
    native <methods>;
    public static java.lang.String ownerOf(boolean, java.lang.String, int, java.lang.String, int);
}
