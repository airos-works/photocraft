// Native keyboard/file-panel integration fixture for PhotoCraft only.
#include <Application.h>
#include <Message.h>
#include <Messenger.h>
#include <InterfaceDefs.h>
#include <stdio.h>
#include <stdlib.h>
int main(int argc,char** argv) {
 if(argc<2)return 2;
 BApplication app("application/x-vnd.PhotoCraft-KeyTest");
 BMessenger target("application/x-vnd.storytold-PhotoCraft");
 BMessage request(B_GET_PROPERTY),reply;
 request.AddSpecifier("Messenger");
 request.AddSpecifier("View",argc>4?argv[4]:"canvas");
 request.AddSpecifier("Window",argc>3?atoi(argv[3]):0);
 if(target.SendMessage(&request,&reply,1000000,1000000)!=B_OK)return 3;
 BMessenger window;
 if(reply.FindMessenger("result",&window)!=B_OK)return 4;
 BMessage key(B_KEY_DOWN);
 key.AddString("bytes",argv[1]);
 key.AddInt32("raw_char",argv[1][0]);
 key.AddInt32("key",argv[1][0]==27?1:40);
 key.AddInt32("modifiers",argc>2?atoi(argv[2]):0);
 key.AddInt64("when",system_time());
 status_t s=window.SendMessage(&key);
 key.what=B_KEY_UP;window.SendMessage(&key);
 printf("native key sent: %ld\n",long(s));
 return s==B_OK?0:5;
}
