/* Paseo's UTF-8 bridge to the same MD4C source used by enriched-markdown.
 * Keep C layout details on this side of the ABI; Rust receives copied
 * scalar/string data. */
#include "md4c.h"
#include <string.h>
typedef void (*PaseoEvent)(void *, unsigned, unsigned, unsigned, const char *,
                           unsigned, const char *, unsigned);
typedef struct {
  PaseoEvent event;
  void *userdata;
} PaseoParser;
static void emit(void *userdata, unsigned event, unsigned type, unsigned number,
                 MD_ATTRIBUTE *a, MD_ATTRIBUTE *b) {
  PaseoParser *p = (PaseoParser *)userdata;
  p->event(p->userdata, event, type, number, a ? a->text : 0, a ? a->size : 0,
           b ? b->text : 0, b ? b->size : 0);
}
static int block_enter(MD_BLOCKTYPE type, void *detail, void *userdata) {
  unsigned number = 0;
  MD_ATTRIBUTE *a = 0;
  if (type == MD_BLOCK_H)
    number = ((MD_BLOCK_H_DETAIL *)detail)->level;
  if (type == MD_BLOCK_OL)
    number = ((MD_BLOCK_OL_DETAIL *)detail)->start;
  if (type == MD_BLOCK_UL)
    number = ((MD_BLOCK_UL_DETAIL *)detail)->is_tight;
  if (type == MD_BLOCK_LI) {
    MD_BLOCK_LI_DETAIL *li = (MD_BLOCK_LI_DETAIL *)detail;
    number = li->is_task ? (li->task_mark == ' ' ? 1 : 2) : 0;
  }
  if (type == MD_BLOCK_CODE)
    a = &((MD_BLOCK_CODE_DETAIL *)detail)->lang;
  if (type == MD_BLOCK_TH || type == MD_BLOCK_TD)
    number = ((MD_BLOCK_TD_DETAIL *)detail)->align;
  if (type == MD_BLOCK_ADMONITION)
    a = &((MD_BLOCK_ADMONITION_DETAIL *)detail)->type;
  emit(userdata, 0, type, number, a, 0);
  return 0;
}
static int block_leave(MD_BLOCKTYPE type, void *detail, void *userdata) {
  (void)detail;
  emit(userdata, 1, type, 0, 0, 0);
  return 0;
}
static int span_enter(MD_SPANTYPE type, void *detail, void *userdata) {
  MD_ATTRIBUTE *a = 0, *b = 0;
  if (type == MD_SPAN_A) {
    a = &((MD_SPAN_A_DETAIL *)detail)->href;
    b = &((MD_SPAN_A_DETAIL *)detail)->title;
  }
  if (type == MD_SPAN_IMG) {
    a = &((MD_SPAN_IMG_DETAIL *)detail)->src;
    b = &((MD_SPAN_IMG_DETAIL *)detail)->title;
  }
  emit(userdata, 2, type, 0, a, b);
  return 0;
}
static int span_leave(MD_SPANTYPE type, void *detail, void *userdata) {
  (void)detail;
  emit(userdata, 3, type, 0, 0, 0);
  return 0;
}
static int text(MD_TEXTTYPE type, const MD_CHAR *text, MD_SIZE size,
                void *userdata) {
  PaseoParser *p = (PaseoParser *)userdata;
  p->event(p->userdata, 4, type, 0, text, size, 0, 0);
  return 0;
}
int paseo_md_parse(const char *text_value, unsigned size, PaseoEvent event,
                   void *userdata) {
  PaseoParser p = {event, userdata};
  MD_PARSER parser = {0};
  /* The app enables GFM, hard soft breaks, autolinks and admonitions; math
   * stays off. Its source adapter escapes spoiler delimiters, so raw native
   * source keeps them literal. */
  parser.flags = MD_FLAG_NOHTMLSPANS | MD_FLAG_TABLES | MD_FLAG_STRIKETHROUGH |
                 MD_FLAG_TASKLISTS | MD_FLAG_PERMISSIVEAUTOLINKS |
                 MD_FLAG_HARD_SOFT_BREAKS | MD_FLAG_ADMONITIONS;
  parser.enter_block = block_enter;
  parser.leave_block = block_leave;
  parser.enter_span = span_enter;
  parser.leave_span = span_leave;
  parser.text = text;
  return md_parse(text_value, size, &parser, &p);
}
