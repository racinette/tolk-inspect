fun narrow(cond: bool): int {
    val value = cond ? 10 : null;
    if (value != null) {
        return value;
    }
    return 0;
}
