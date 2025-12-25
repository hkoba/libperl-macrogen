#define SHIFT 0
#define FLAG (1U << (SHIFT+1))
#if FLAG != 2
#error test failed
#endif
int x;
