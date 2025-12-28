// Test: variable shadowing
int x = 1;
int test(void) {
    int x = 2;  // shadows global x
    return x;   // should be local x (int)
}
